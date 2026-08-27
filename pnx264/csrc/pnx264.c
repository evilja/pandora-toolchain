#include "pnx264.h"
#include <x264.h>
#include <stdlib.h>
#include <string.h>

struct pnx264_enc {
    x264_t *h;
    x264_picture_t pic_in;
    x264_picture_t pic_out;
    int64_t next_pts;
};

/* "x264-params" is not a key libx264 knows — it is an ffmpeg-level convenience that ffmpeg
 * expands itself, splitting on ':' and calling x264_param_parse() per pair. Reproduced here so
 * the -x264-params strings in src/lib/mpeg/preset.rs mean the same thing through the FFI as
 * they do through pnmpeg. A bare token with no '=' is passed as "1", matching ffmpeg.
 *
 * Limitation, shared with ffmpeg's own parser: a value cannot contain ':' because that is the
 * separator, so options like psy-rd=1.00:0.00 cannot be expressed. None of the current presets
 * need one. */
static int parse_x264_params( x264_param_t *p, const char *params )
{
    char *dup = strdup( params );
    if( !dup )
        return -1;
    int rc = 0;
    char *save = NULL;
    for( char *tok = strtok_r( dup, ":", &save ); tok; tok = strtok_r( NULL, ":", &save ) )
    {
        char *eq = strchr( tok, '=' );
        if( eq )
        {
            *eq = '\0';
            rc = x264_param_parse( p, tok, eq + 1 );
        }
        else
            rc = x264_param_parse( p, tok, "1" );
        if( rc < 0 )
            break;
    }
    free( dup );
    return rc;
}

/* x264 guarantees the payloads of nal[0..i_nal-1] are contiguous in one buffer, so a caller
 * can take nal[0].p_payload plus the summed length rather than stitching them together. */
static int collect( x264_nal_t *nal, int i_nal, const uint8_t **out )
{
    int size = 0;
    for( int i = 0; i < i_nal; i++ )
        size += nal[i].i_payload;
    *out = i_nal > 0 ? nal[0].p_payload : NULL;
    return size;
}

pnx264_enc *pnx264_open( const pnx264_config *cfg, const char **err )
{
    x264_param_t p;
    if( x264_param_default_preset( &p, cfg->preset, cfg->tune ) < 0 )
    {
        if( err ) *err = "unknown preset or tune";
        return NULL;
    }

    p.i_csp         = X264_CSP_I420;
    p.i_width       = cfg->width;
    p.i_height      = cfg->height;
    p.i_fps_num     = cfg->fps_num;
    p.i_fps_den     = cfg->fps_den;
    p.i_timebase_num = cfg->fps_den;
    p.i_timebase_den = cfg->fps_num;
    /* Deliberately NOT forcing b_vfr_input = 0. It only reaches the bitstream as the SPS VUI
     * fixed_frame_rate_flag, but ffmpeg leaves x264's default (VFR) in place and production
     * output comes from ffmpeg, so forcing CFR here flips exactly one bit and breaks the
     * byte-for-byte invariant everything downstream is checked against. Input is CFR either
     * way: pts advances by one tick per frame against a 1/fps timebase, so frame durations —
     * and therefore the mbtree qscale, which is a function of duration — are unaffected. */
    p.i_threads     = cfg->threads > 0 ? cfg->threads : X264_THREADS_AUTO;
    /* Annex-B with per-frame headers off: chunks are concatenated as elementary streams and
     * every chunk starts on its own IDR, so repeating SPS/PPS per keyframe would be the only
     * way to make a mid-stream chunk decodable on its own. Keep it explicit rather than
     * inheriting whatever the preset left behind. */
    p.b_annexb      = 1;
    p.b_repeat_headers = 1;

    /* Plan-only runs the lookahead and reports frame types without coding anything. The
     * ratecontrol and preset settings still matter: scenecut and b-adapt decisions depend on
     * them, so the plan is only valid for the preset it was produced with.
     *
     * Keep ordinary encoding buildable against distro x264: only the Pandora header owns this
     * appended field, and advertises it explicitly rather than making the shim guess from the
     * upstream build number (which intentionally remains 165). */
#ifdef X264_PANDORA_PLAN_ONLY
    p.b_plan_only = cfg->plan_only;
#else
    if( cfg->plan_only )
    {
        if( err ) *err = "plan-only requires the Pandora x264 fork";
        return NULL;
    }
#endif

    p.rc.i_rc_method  = X264_RC_CRF;
    p.rc.f_rf_constant = cfg->crf;

    if( cfg->level && x264_param_parse( &p, "level", cfg->level ) < 0 )
    {
        if( err ) *err = "bad level";
        return NULL;
    }

    int ref_before_params = p.i_frame_reference;
    if( cfg->x264_params && parse_x264_params( &p, cfg->x264_params ) < 0 )
    {
        if( err ) *err = "bad x264-params";
        return NULL;
    }

    /* Reduce the reference count to what the target level's DPB allows.
     *
     * libx264 does NOT do this: validate_parameters() only clips against X264_REF_MAX. Both
     * the x264 CLI (x264.c, "Automatically reduce reference frame count") and ffmpeg's
     * libx264 wrapper implement it themselves, which is why an FFI encoder that skips it
     * silently disagrees with both. Concretely, `veryslow` at 1080p wants ref=16 while level
     * 4.1 permits 4 (MaxDpbMbs 32768 / 8040 MBs per frame), and the resulting stream both
     * differs from production output and overruns the level it declares.
     *
     * Matches the CLI's condition: skipped when the caller set refs explicitly. */
    if( cfg->level && p.i_frame_reference == ref_before_params )
    {
        int mbs = ((cfg->width + 15) >> 4) * ((cfg->height + 15) >> 4);
        for( int i = 0; x264_levels[i].level_idc != 0; i++ )
            if( p.i_level_idc == x264_levels[i].level_idc )
            {
                while( mbs * p.i_frame_reference > x264_levels[i].dpb && p.i_frame_reference > 1 )
                    p.i_frame_reference--;
                break;
            }
    }
    if( cfg->profile && x264_param_apply_profile( &p, cfg->profile ) < 0 )
    {
        if( err ) *err = "bad profile";
        return NULL;
    }

    pnx264_enc *e = calloc( 1, sizeof(*e) );
    if( !e )
    {
        if( err ) *err = "out of memory";
        return NULL;
    }
    e->h = x264_encoder_open( &p );
    if( !e->h )
    {
        free( e );
        if( err ) *err = "x264_encoder_open failed";
        return NULL;
    }
    x264_picture_init( &e->pic_in );
    x264_picture_init( &e->pic_out );
    e->pic_in.img.i_csp = X264_CSP_I420;
    e->pic_in.img.i_plane = 3;
    return e;
}

int pnx264_headers( pnx264_enc *e, const uint8_t **out )
{
    x264_nal_t *nal;
    int i_nal;
    if( x264_encoder_headers( e->h, &nal, &i_nal ) < 0 )
        return -1;
    return collect( nal, i_nal, out );
}

int pnx264_encode( pnx264_enc *e,
                   const uint8_t *y, const uint8_t *u, const uint8_t *v,
                   int stride_y, int stride_u, int stride_v,
                   int64_t pts, const uint8_t **out )
{
    x264_nal_t *nal;
    int i_nal;
    /* x264 does not write through these, but the public img.plane is non-const. */
    e->pic_in.img.plane[0]  = (uint8_t *)y;
    e->pic_in.img.plane[1]  = (uint8_t *)u;
    e->pic_in.img.plane[2]  = (uint8_t *)v;
    e->pic_in.img.i_stride[0] = stride_y;
    e->pic_in.img.i_stride[1] = stride_u;
    e->pic_in.img.i_stride[2] = stride_v;
    e->pic_in.i_pts = pts;
    e->pic_in.i_type = X264_TYPE_AUTO;

    int size = x264_encoder_encode( e->h, &nal, &i_nal, &e->pic_in, &e->pic_out );
    if( size < 0 )
        return -1;
    return collect( nal, i_nal, out );
}

int pnx264_flush( pnx264_enc *e, const uint8_t **out )
{
    x264_nal_t *nal;
    int i_nal;
    if( !x264_encoder_delayed_frames( e->h ) )
        return 0;
    int size = x264_encoder_encode( e->h, &nal, &i_nal, NULL, &e->pic_out );
    if( size < 0 )
        return -1;
    return collect( nal, i_nal, out );
}

static int fill_plan( pnx264_enc *e, pnx264_plan_entry *out )
{
    /* encoder_encode() leaves i_type as X264_TYPE_AUTO when the frame is still buffered in
     * the lookahead and no decision has been made yet. */
    if( e->pic_out.i_type == X264_TYPE_AUTO )
        return 0;
    out->frame_type = e->pic_out.i_type;
    out->keyframe   = e->pic_out.b_keyframe;
    out->is_idr     = e->pic_out.i_type == X264_TYPE_IDR;
    out->pts        = e->pic_out.i_pts;
    return 1;
}

int pnx264_plan_push( pnx264_enc *e,
                      const uint8_t *y, const uint8_t *u, const uint8_t *v,
                      int stride_y, int stride_u, int stride_v,
                      int64_t pts, pnx264_plan_entry *out )
{
    x264_nal_t *nal;
    int i_nal;
    e->pic_in.img.plane[0]  = (uint8_t *)y;
    e->pic_in.img.plane[1]  = (uint8_t *)u;
    e->pic_in.img.plane[2]  = (uint8_t *)v;
    e->pic_in.img.i_stride[0] = stride_y;
    e->pic_in.img.i_stride[1] = stride_u;
    e->pic_in.img.i_stride[2] = stride_v;
    e->pic_in.i_pts = pts;
    e->pic_in.i_type = X264_TYPE_AUTO;
    e->pic_out.i_type = X264_TYPE_AUTO;

    if( x264_encoder_encode( e->h, &nal, &i_nal, &e->pic_in, &e->pic_out ) < 0 )
        return -1;
    return fill_plan( e, out );
}

int pnx264_plan_flush( pnx264_enc *e, pnx264_plan_entry *out )
{
    x264_nal_t *nal;
    int i_nal;
    if( !x264_encoder_delayed_frames( e->h ) )
        return 0;
    e->pic_out.i_type = X264_TYPE_AUTO;
    if( x264_encoder_encode( e->h, &nal, &i_nal, NULL, &e->pic_out ) < 0 )
        return -1;
    return fill_plan( e, out );
}

void pnx264_close( pnx264_enc *e )
{
    if( !e )
        return;
    if( e->h )
        x264_encoder_close( e->h );
    free( e );
}
