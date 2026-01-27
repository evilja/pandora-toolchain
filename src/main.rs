use pandora_toolchain::libpnmpeg::probe::ffprobe;

fn main() {
    println!("{:?}", ffprobe("a.mkv", "jpn"));
}