/* Narrow C shim over libx264.
 *
 * Rust never sees x264_param_t. That struct is large, changes shape between builds, and
 * carries unions and function pointers; replicating its ABI on the Rust side is the usual
 * way to get silent memory corruption when the system library moves. Here the C compiler
 * owns every x264 layout and Rust only ever passes primitives and opaque pointers.
 *
 * This is also where the plan-emit / plan-replay entry points will go once the forked
 * encoder grows them, so the Rust side never has to learn about ratecontrol internals. */
#ifndef PNX264_H
#define PNX264_H

#include <stdint.h>
#include <stddef.h>

typedef struct pnx264_enc pnx264_enc;

typedef struct {
    int width;
    int height;
    int fps_num;
    int fps_den;
    float crf;
    int threads;          /* 0 = x264 chooses */
    const char *preset;   /* NULL for x264 default */
    const char *tune;     /* NULL for none */
    const char *profile;  /* NULL for none, e.g. "high" */
    const char *level;    /* NULL for none, e.g. "4.1" */
    const char *x264_params; /* NULL, or "key=value:key=value" as -x264-params */
    int plan_only;        /* 1 = lookahead only, no coding; requires the Pandora x264 fork */
} pnx264_config;

/* One frame's planning result. i_type uses x264's X264_TYPE_* values. */
typedef struct {
    int     frame_type;
    int     keyframe;     /* 1 = starts a new GOP; an IDR here is a safe chunk boundary */
    int     is_idr;
    int64_t pts;
} pnx264_plan_entry;

/* Returns NULL on failure; err (if non-NULL) receives a static description. */
pnx264_enc *pnx264_open(const pnx264_config *cfg, const char **err);

/* Emit SPS/PPS/SEI. Sets *out to an internal buffer valid until the next call. */
int pnx264_headers(pnx264_enc *e, const uint8_t **out);

/* Encode one yuv420p frame. Returns bytes in *out (0 = frame buffered, no output yet),
 * or negative on error. *out stays valid until the next call on this encoder. */
int pnx264_encode(pnx264_enc *e,
                  const uint8_t *y, const uint8_t *u, const uint8_t *v,
                  int stride_y, int stride_u, int stride_v,
                  int64_t pts, const uint8_t **out);

/* Drain one buffered frame. Returns 0 when the encoder is empty. */
int pnx264_flush(pnx264_enc *e, const uint8_t **out);

/* Plan-only counterparts of encode/flush. Return 1 when *out was filled, 0 when the frame is
 * still inside the lookahead and nothing was decided yet, negative on error. */
int pnx264_plan_push(pnx264_enc *e,
                     const uint8_t *y, const uint8_t *u, const uint8_t *v,
                     int stride_y, int stride_u, int stride_v,
                     int64_t pts, pnx264_plan_entry *out);
int pnx264_plan_flush(pnx264_enc *e, pnx264_plan_entry *out);

void pnx264_close(pnx264_enc *e);

#endif
