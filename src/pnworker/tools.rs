// pnworker/tools.rs
use crate::pnworker::util::CliParam;

pub const PNCURL_TORRENT: &[CliParam] = &[
    CliParam::Literal("--link"),        CliParam::Path("LINK"),
    CliParam::Literal("--opcode"),      CliParam::Path("OPCODE"),
    CliParam::Literal("--negkey"),      CliParam::JobId("PNcurlT"),
    CliParam::Literal("--negotiator"),  CliParam::Literal("PNdloadworker"),
    CliParam::Literal("--negver"),      CliParam::NegVer("1"),
    CliParam::Literal("--logfile"),     CliParam::Path("LOGFILE"),
];

pub const PNP2P_TORRENT: &[CliParam] = &[
    CliParam::Literal("--opcode"),      CliParam::Path("OPCODE"),
    CliParam::Literal("--save"),        CliParam::Path("SAVE"),
    CliParam::Literal("--negkey"),      CliParam::JobId("PNp2pT"),
    CliParam::Literal("--negotiator"),  CliParam::Literal("PNdloadworker"),
    CliParam::Literal("--negver"),      CliParam::NegVer("1"),
    CliParam::Literal("--cancelfile"),  CliParam::Path("CANCELFILE"),
];

pub const PNMPEG_ENCODE: &[CliParam] = &[
    CliParam::Literal("--input"),       CliParam::Path("INPUT"),
    CliParam::Literal("--output"),      CliParam::Path("OUTPUT"),
    CliParam::Literal("--ass"),         CliParam::Path("ASS"),
    CliParam::Path("PRESET"),
    CliParam::Literal("--negkey"),      CliParam::JobId("PNmpeg"),
    CliParam::Literal("--negotiator"),  CliParam::Literal("PNencdeworker"),
    CliParam::Literal("--negver"),      CliParam::NegVer("1"),
    CliParam::Literal("--cancelfile"),  CliParam::Path("CANCELFILE"),
    CliParam::Literal("--logfile"),     CliParam::Path("LOGFILE"),
];

pub const PNMPEG_CONCAT: &[CliParam] = &[
    CliParam::Literal("--input"),       CliParam::Path("INPUT"),
    CliParam::Literal("--output"),      CliParam::Path("OUTPUT"),
    CliParam::Literal("--concat"),
    CliParam::RepeatedPath("CANDIDATES"),
    CliParam::Literal("--negkey"),      CliParam::JobId("PNmpegC"),
    CliParam::Literal("--negotiator"),  CliParam::Literal("PNencdeworker"),
    CliParam::Literal("--negver"),      CliParam::NegVer("1"),
    CliParam::Literal("--cancelfile"),  CliParam::Path("CANCELFILE"),
    CliParam::Literal("--logfile"),     CliParam::Path("LOGFILE"),
];

pub const PNCURL_UPLOAD: &[CliParam] = &[
    CliParam::Literal("--link"),        CliParam::Path("LINK"),
    CliParam::Literal("--opcode"),      CliParam::Path("OPCODE"),
    CliParam::Literal("--drive"),
    CliParam::Literal("--env"),         CliParam::Literal("env.pandora"),
    CliParam::Literal("--negkey"),      CliParam::JobId("PNcurlG"),
    CliParam::Literal("--negotiator"),  CliParam::Literal("PNuloadworker"),
    CliParam::Literal("--negver"),      CliParam::NegVer("1"),
];
