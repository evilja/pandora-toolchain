// pnworker/tools.rs
use crate::libpnenv::standard::ENV_PATH;
use crate::pnworker::util::CliParam;

pub const PNCURL_TORRENT: &[CliParam] = &[
    CliParam::Literal("--link"),        CliParam::Path("LINK"),
    CliParam::Literal("--opcode"),      CliParam::Path("OPCODE"),
    CliParam::Literal("--negkey"),      CliParam::JobId("PNcurlT"),
    CliParam::Literal("--negotiator"),  CliParam::Literal("PNdloadworker"),
    CliParam::Literal("--negver"),      CliParam::NegVer("1"),
    CliParam::Literal("--logfile"),     CliParam::Path("LOGFILE"),
];

pub const PNCURL_GSCRAPE: &[CliParam] = &[
    CliParam::Literal("--link"),        CliParam::Path("LINK"),
    CliParam::Literal("--opcode"),      CliParam::Path("OPCODE"),
    CliParam::Literal("--gscrape"),
    CliParam::Literal("--negkey"),      CliParam::JobId("PNcurlGS"),
    CliParam::Literal("--negotiator"),  CliParam::Literal("PNdloadworker"),
    CliParam::Literal("--negver"),      CliParam::NegVer("1"),
    CliParam::Literal("--logfile"),     CliParam::Path("LOGFILE"),
    CliParam::Literal("--cancelfile"),  CliParam::Path("CANCELFILE"),
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
    CliParam::Literal("--env"),         CliParam::Literal(ENV_PATH),
    CliParam::Literal("--negkey"),      CliParam::JobId("PNcurlG"),
    CliParam::Literal("--negotiator"),  CliParam::Literal("PNuloadworker"),
    CliParam::Literal("--negver"),      CliParam::NegVer("1"),
];

pub const PNCURL_BACKUP: &[CliParam] = &[
    CliParam::Literal("--link"),        CliParam::Path("LINK"),
    CliParam::Literal("--opcode"),      CliParam::Path("OPCODE"),
    CliParam::Literal("--drive"),
    CliParam::Literal("--env"),         CliParam::Literal(ENV_PATH),
    CliParam::Literal("--negkey"),      CliParam::JobId("PNcurlG"),
    CliParam::Literal("--negotiator"),  CliParam::Literal("PNuloadworker"),
    CliParam::Literal("--negver"),      CliParam::NegVer("1"),
    CliParam::Literal("--backup"),
];

pub const PNP2P_PROBE: &[CliParam] = &[
    CliParam::Literal("--opcode"),      CliParam::Path("OPCODE"),
    CliParam::Literal("--negkey"),      CliParam::JobId("PNp2pP"),
    CliParam::Literal("--negotiator"),  CliParam::Literal("PNprobeworker"),
    CliParam::Literal("--negver"),      CliParam::NegVer("1"),
    CliParam::Literal("--probe"),
];

pub const PNP2P_SELECT: &[CliParam] = &[
    CliParam::Literal("--opcode"),      CliParam::Path("OPCODE"),
    CliParam::Literal("--save"),        CliParam::Path("SAVE"),
    CliParam::Literal("--select"),      CliParam::Path("INDEX"),
    CliParam::Literal("--negkey"),      CliParam::JobId("PNp2pS"),
    CliParam::Literal("--negotiator"),  CliParam::Literal("PNdloadworker"),
    CliParam::Literal("--negver"),      CliParam::NegVer("1"),
    CliParam::Literal("--cancelfile"),  CliParam::Path("CANCELFILE"),
];

pub const PNASS_LAYER: &[CliParam] = &[
    CliParam::Literal("--input"),        CliParam::Path("INPUT"),
    CliParam::Literal("--output"),       CliParam::Path("OUTPUT"),
    CliParam::Literal("--set-layer"),    CliParam::Literal("9"),
    CliParam::Literal("--negkey"),       CliParam::Literal("PNass"),
    CliParam::Literal("--negotiator"),   CliParam::Literal("PNdc"),
    CliParam::Literal("--negver"),       CliParam::Literal("0.1.1"),
];
