use pandora_toolchain::{libpnprotocol::{core::{Protocol, Schema}}, pn_emit, pn_data, pn_schema};

fn main(){
    let mut proto = Protocol::new(vec![1]);
    proto.negotiate("PNprotocol:PNcurl@1@1:PNcurl@1@1:KEY").unwrap();
    let neg = "KEY";
    let a = pn_emit!(
        protocol = proto,
        negkey = &neg,
        schema = [leaf, leaf],
        data   = [":?PNslash?PNquestion??%/", "???aaaa/%?PNslash?"]
    ).unwrap();
    println!("{}", a);
    println!("{:?}", proto.extract_data(&a).unwrap());
}
