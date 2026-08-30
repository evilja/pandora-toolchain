// Q: gdrive_client_id, gdrive_client_secret, gdrive_refresh_token, gdrive_token_url, discord_token, gdrive_upload_url, pnmpeg, pnp2p, pncurl, gdrive_parent_id, doodstream, uqload, lulu, voesx, abyss, pnass
//

pub const ENV_PATH: &str = "DB/config/global/environment/env.pandora";

pub const ENV_SEP: &str = "|pntools|";

pub const CLIENT_ID: &str = "gdrive_client_id";
pub const CLIENT_SECRET: &str = "gdrive_client_secret";
pub const REFRESH_TOKEN: &str = "gdrive_refresh_token";
pub const TOKEN_URL: &str = "gdrive_token_url";
pub const TOKEN: &str = "discord_token";
pub const UPLOAD_URL: &str = "gdrive_upload_url";
pub const PNMPEG: &str = "pnmpeg";
pub const PNP2P: &str = "pnp2p";
pub const PNCURL: &str = "pncurl";
pub const PARENTID: &str = "gdrive_parent_id";
pub const DOODSTREAM: &str = "doodstream";
pub const UQLOAD: &str = "uqload";
pub const LULU: &str = "lulu";
pub const VOESX: &str = "voesx";
pub const ABYSS: &str = "abyss";
pub const PNASS: &str = "pnass";
pub const ANISUB: &str = "anisub";
pub const ANIMECIX: &str = "animecix";
pub const ANIMECIX_EMAIL: &str = "animecix_email";
pub const ANIMECIX_PASSWORD: &str = "animecix_password";
pub const OPENANIME_EMAIL: &str = "openanime_email";
pub const OPENANIME_PASSWORD: &str = "openanime_password";
pub const ANIZM_EMAIL: &str = "anizm_email";
pub const ANIZM_PASSWORD: &str = "anizm_password";
pub const AKIRA_API: &str = "akira_api";
pub const AKIRA_TOKEN: &str = "akira_token";
pub const AKIRA_INDEX: &str = "akira_index";

pub const LUMIERE_BROKER_URL: &str = "lumiere_broker_url";
pub const LUMIERE_BROKER_TOKEN: &str = "lumiere_broker_token";
pub const LUMIERE_PUBLIC_URL: &str = "lumiere_public_url";
pub const LUMIERE_TRANSFER_TTL_SECS: &str = "lumiere_transfer_ttl_secs";
pub const LUMIERE_POLL_INTERVAL_SECS: &str = "lumiere_poll_interval_secs";
pub const LUMIERE_LOG_VERBOSE: &str = "lumiere_log_verbose";
pub const LUMIERE_REMOTE_STALL_SECS: &str = "lumiere_remote_stall_secs";

pub const API_PORT: &str = "api_port";
pub const API_HOST: &str = "api_host";
pub const API_AUTHOR_ID: &str = "api_author_id";
pub const API_PUBLIC_URL: &str = "api_public_url";
pub const API_RATE_LIMIT: &str = "api_rate_limit";
pub const API_RATE_WINDOW_SECS: &str = "api_rate_window_secs";
pub const API_TOKENS_PATH: &str = "DB/config/global/environment/api.pandora";
pub const FLAVOR_PATH: &str = "DB/config/global/environment/flavor.pandora";

// Pandora Mini link. `PANDORA_MODE` set to `mini` runs the node side (equivalently `pndc --mini`):
// the worker runtime and a link client, no Discord. Everything else is coordinator-side, and a
// coordinator with no registered nodes behaves exactly as it did before the link existed.
pub const PANDORA_MODE: &str = "pandora_mode";
pub const LINK_COORDINATOR_URL: &str = "link_coordinator_url";
pub const LINK_NODE_TOKEN: &str = "link_node_token";
pub const LINK_NODE_NAME: &str = "link_node_name";
pub const LINK_MAX_JOBS: &str = "link_max_jobs";
pub const LINK_ENABLED: &str = "link_enabled";
pub const LINK_ONLY_NODE: &str = "link_only_node";
pub const LINK_LEASE_TIMEOUT_SECS: &str = "link_lease_timeout_secs";
pub const LINK_ALLOW_BUILD_MISMATCH: &str = "link_allow_build_mismatch";
pub const LINK_NODES_PATH: &str = "DB/config/global/environment/link_nodes.json";
