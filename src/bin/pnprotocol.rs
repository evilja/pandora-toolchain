use pandora_toolchain::libpnprotocol::core::{Protocol, TypeC};

fn main(){
    let mut proto = Protocol::new(vec![1]);
    proto.negotiate("PNprotocol:PNcurl@1@1:PNcurl@1@1:KEY").unwrap();
    println!("{:?}", proto.extract_data("KEY:0:1").unwrap());
}
