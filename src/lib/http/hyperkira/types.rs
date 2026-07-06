use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::defaults::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthRead {
    pub status: String,
    pub app: String,
    pub environment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimeStats {
    pub total_animes: i64,
    pub total_episodes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicSettingsRead {
    pub registration_enabled: bool,
    pub registration_mode: String,
    pub comments_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SiteSettingsUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments_enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentsSettingsRead {
    pub comments_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentsSettingsUpdate {
    pub comments_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecruitmentRole {
    pub id: String,
    pub title: String,
    pub is_open: bool,
    pub description: String,
    pub requirements: Vec<String>,
    pub apply_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecruitmentSettings {
    pub page_subtitle: String,
    pub general_requirements: Vec<String>,
    pub roles: Vec<RecruitmentRole>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaffRolesSettings {
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RegistrationResponse {
    Token(TokenResponse),
    Pending(RegistrationPendingResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub remember_me: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invite_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub user: UserRead,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationPendingResponse {
    pub status: String,
    pub message: String,
    pub user: UserRead,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRead {
    pub id: i64,
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_url: Option<String>,
    pub role: String,
    pub is_admin: bool,
    pub is_active: bool,
    pub profile_is_public: bool,
    pub track_history: bool,
    pub created_at: String,
    pub updated_at: String,
    pub last_login_at: Option<String>,
    pub last_seen_at: Option<String>,
    pub is_online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPage {
    pub items: Vec<UserRead>,
    pub total: i64,
    pub page: i64,
    pub size: i64,
    pub page_size: i64,
    pub pages: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserMeUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_is_public: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_history: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserCreate {
    pub username: String,
    pub password: String,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default = "default_member_role")]
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_admin: Option<bool>,
    #[serde(default = "default_true")]
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_admin: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_history: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPublicRead {
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub bio: Option<String>,
    pub role: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityRating {
    pub average: Option<f64>,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimeRatingWrite {
    pub score: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimeRatingRead {
    pub average: Option<f64>,
    pub count: i64,
    pub user_score: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimeExternalLinkRead {
    pub kind: String,
    pub label: String,
    pub url: String,
    pub sort_order: i64,
    pub generated: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimeFranchiseRelationRead {
    pub id: i64,
    pub relation_type: String,
    pub related_mal_id: Option<i64>,
    pub related_slug: Option<String>,
    pub title: String,
    pub url: Option<String>,
    pub cover_image_url: Option<String>,
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimeBroadcastRead {
    pub id: i64,
    pub source: String,
    pub day: Option<String>,
    pub time: Option<String>,
    pub timezone: Option<String>,
    pub broadcast_string: Option<String>,
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimeListItem {
    pub id: i64,
    pub slug: String,
    pub title_tr: String,
    pub title_jp: Option<String>,
    pub title_en: Option<String>,
    pub cover_image_url: Option<String>,
    pub trailer_url: Option<String>,
    pub current_episode: f64,
    pub total_episodes: Option<i64>,
    pub status: String,
    pub mal_score: Option<f64>,
    pub age_rating: Option<String>,
    pub episode_duration_minutes: Option<i64>,
    pub media_type: Option<String>,
    pub quality_badge: Option<String>,
    pub producer: Option<String>,
    pub season_label: Option<String>,
    pub demographic: Option<String>,
    pub comments_enabled: bool,
    pub tags: Vec<String>,
    pub community_rating: CommunityRating,
    pub featured_order: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimeDetail {
    #[serde(flatten)]
    pub item: AnimeListItem,
    pub synopsis: Option<String>,
    pub banner_image_url: Option<String>,
    pub drive_index_url: Option<String>,
    pub platform_slug: Option<String>,
    pub animecix_id: Option<String>,
    pub disabled_platforms: Vec<String>,
    pub external_links: Vec<AnimeExternalLinkRead>,
    pub default_staff: Vec<EpisodeStaffCreditRead>,
    pub franchise_relations: Vec<AnimeFranchiseRelationRead>,
    pub broadcasts: Vec<AnimeBroadcastRead>,
    pub recommendations: Vec<AnimeListItem>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimePage {
    pub items: Vec<AnimeListItem>,
    pub total: i64,
    pub page: i64,
    pub size: i64,
    pub page_size: i64,
    pub pages: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeLinkRead {
    pub kind: String,
    pub label: String,
    pub url: String,
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeStaffCreditRead {
    pub role: String,
    pub name: String,
    pub crew_member_id: Option<i64>,
    pub crew_member: Option<CrewMemberRead>,
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeRead {
    pub id: i64,
    pub episode_number: f64,
    pub title: Option<String>,
    pub player_embed_url: Option<String>,
    pub goindex_url: Option<String>,
    pub comments_enabled: bool,
    pub released_at: Option<String>,
    pub links: Vec<EpisodeLinkRead>,
    pub staff_credits: Vec<EpisodeStaffCreditRead>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodePage {
    pub items: Vec<EpisodeRead>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub pages: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalLinkWrite {
    pub kind: String,
    pub label: String,
    pub url: String,
    #[serde(default)]
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FranchiseRelationWrite {
    pub relation_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_mal_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_slug: Option<String>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastWrite {
    #[serde(default = "default_jikan")]
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broadcast_string: Option<String>,
    #[serde(default)]
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeLinkWrite {
    pub kind: String,
    pub label: String,
    pub url: String,
    #[serde(default)]
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformLinksWrite {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub urls: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<ExternalLinkWrite>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeStaffCreditWrite {
    pub role: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crew_member_id: Option<i64>,
    #[serde(default)]
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimeCreate {
    pub slug: String,
    pub title_tr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_jp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synopsis: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_image_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub banner_image_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trailer_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drive_index_url: Option<String>,
    #[serde(default)]
    pub current_episode: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_episodes: Option<i64>,
    #[serde(default = "default_ongoing")]
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mal_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_rating: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_duration_minutes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_en: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_badge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub demographic: Option<String>,
    #[serde(default = "default_true")]
    pub comments_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animecix_id: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub external_links: Vec<ExternalLinkWrite>,
    #[serde(default)]
    pub franchise_relations: Vec<FranchiseRelationWrite>,
    #[serde(default)]
    pub broadcasts: Vec<BroadcastWrite>,
    #[serde(default)]
    pub default_staff: Vec<EpisodeStaffCreditWrite>,
    #[serde(default)]
    pub skip_webhook: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AnimeUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_tr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_jp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synopsis: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_image_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub banner_image_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trailer_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drive_index_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_episode: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_episodes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mal_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_rating: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_duration_minutes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_en: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_badge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub demographic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animecix_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_platforms: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub featured_order: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeCreate {
    pub episode_number: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player_embed_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goindex_url: Option<String>,
    #[serde(default = "default_true")]
    pub comments_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub released_at: Option<String>,
    #[serde(default)]
    pub links: Vec<EpisodeLinkWrite>,
    #[serde(default)]
    pub staff_credits: Vec<EpisodeStaffCreditWrite>,
    #[serde(default = "default_admin")]
    pub source: String,
    #[serde(default)]
    pub skip_webhook: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EpisodeUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player_embed_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goindex_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub released_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeCountUpdate {
    pub current_episode: f64,
    #[serde(default = "default_admin")]
    pub source: String,
    #[serde(default)]
    pub skip_webhook: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseEventRead {
    pub id: i64,
    pub episode_number: f64,
    pub source: String,
    pub webhook_status: String,
    pub webhook_error: Option<String>,
    pub created_at: String,
    pub sent_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeWriteResponse {
    pub episode_number: f64,
    pub release_event: Option<ReleaseEventRead>,
    pub duplicate_release: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimeEventRead {
    pub id: i64,
    pub event_type: String,
    pub source: String,
    pub webhook_status: String,
    pub webhook_error: Option<String>,
    pub created_at: String,
    pub sent_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnounceRequest {
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturedOrderItem {
    pub slug: String,
    pub featured_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturedOrderUpdate {
    pub orders: Vec<FeaturedOrderItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchEpisodeAdd {
    pub episodes: Vec<f64>,
    #[serde(default)]
    pub skip_webhook: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchEpisodeDelete {
    pub episodes: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchAnimeDelete {
    pub slugs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrewMemberBase {
    pub name: String,
    pub role: Option<String>,
    pub bio: Option<String>,
    pub avatar_url: Option<String>,
    pub social_links_json: String,
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrewMemberCreate {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(default = "default_empty_array_json")]
    pub social_links_json: String,
    #[serde(default)]
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CrewMemberUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub social_links_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrewMemberRead {
    #[serde(flatten)]
    pub base: CrewMemberBase,
    pub id: i64,
    pub created_at: String,
    pub updated_at: String,
    pub episode_count: i64,
    pub series_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrewRoleStat {
    pub role: String,
    pub episode_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrewStatsRead {
    pub id: i64,
    pub name: String,
    pub avatar_url: Option<String>,
    pub episode_count: i64,
    pub series_count: i64,
    pub roles: Vec<CrewRoleStat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnouncementCreate {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(rename = "type", default = "default_info")]
    pub kind: String,
    #[serde(default = "default_true")]
    pub is_active: bool,
    #[serde(default)]
    pub show_on_all_pages: bool,
    #[serde(default)]
    pub persistent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AnnouncementUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "type")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_on_all_pages: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistent: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnouncementRead {
    pub id: i64,
    pub message: String,
    pub url: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub is_active: bool,
    pub show_on_all_pages: bool,
    pub persistent: bool,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteCodeCreate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default = "default_one")]
    pub count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default = "default_member_role")]
    pub grants_role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default = "default_true")]
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InviteCodeUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grants_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteCodeRead {
    pub id: i64,
    pub code: String,
    pub created_by: Option<i64>,
    pub used_by: Option<i64>,
    pub created_at: String,
    pub used_at: Option<String>,
    pub is_used: bool,
    pub max_uses: Option<i64>,
    pub expires_at: Option<String>,
    pub grants_role: String,
    pub note: Option<String>,
    pub is_active: bool,
    pub usage_count: i64,
    pub is_expired: bool,
    pub is_exhausted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteCodePage {
    pub items: Vec<InviteCodeRead>,
    pub total: i64,
    pub page: i64,
    pub size: i64,
    pub page_size: i64,
    pub pages: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InviteCodeBatchDelete {
    pub ids: Vec<i64>,
    pub codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteCodeBatchDeleteResponse {
    pub deleted: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchHistoryUpsert {
    pub episode_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchHistoryRead {
    pub id: i64,
    pub user_id: i64,
    pub episode_id: i64,
    pub last_watched_at: String,
    pub anime_title: String,
    pub anime_slug: String,
    pub episode_number: f64,
    pub anime_cover_url: Option<String>,
    pub watched_episode_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchlistCreate {
    pub anime_id: i64,
    #[serde(default = "default_plan_to_watch")]
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchlistUpdate {
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchlistRead {
    pub id: i64,
    pub user_id: i64,
    pub anime_id: i64,
    pub status: String,
    pub added_at: String,
    pub anime_title: String,
    pub anime_slug: String,
    pub anime_cover_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinueWatchingItem {
    pub anime_slug: String,
    pub anime_title: String,
    pub anime_cover_url: Option<String>,
    pub next_episode_number: f64,
    pub total_episodes: Option<i64>,
    pub current_episode: Option<f64>,
    pub last_watched_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchlistPage {
    pub items: Vec<WatchlistRead>,
    pub total: i64,
    pub page: i64,
    pub size: i64,
    pub page_size: i64,
    pub pages: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentUserRead {
    pub id: i64,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentAdminUserRead {
    pub id: i64,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentCreate {
    pub body: String,
    #[serde(default)]
    pub is_spoiler: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_number: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommentUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_spoiler: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentModerationUpdate {
    pub is_hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentVoteCreate {
    pub vote: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentRead {
    pub id: i64,
    pub anime_id: i64,
    pub episode_id: Option<i64>,
    pub parent_id: Option<i64>,
    pub body: String,
    pub is_spoiler: bool,
    pub is_hidden: bool,
    pub created_at: String,
    pub updated_at: String,
    pub user: CommentUserRead,
    pub like_count: i64,
    pub dislike_count: i64,
    pub user_vote: Option<i64>,
    pub replies: Vec<CommentRead>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentPage {
    pub items: Vec<CommentRead>,
    pub total: i64,
    pub page: i64,
    pub size: i64,
    pub page_size: i64,
    pub pages: i64,
    pub comments_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentAdminRead {
    pub id: i64,
    pub anime_id: i64,
    pub episode_id: Option<i64>,
    pub parent_id: Option<i64>,
    pub body: String,
    pub is_spoiler: bool,
    pub is_hidden: bool,
    pub created_at: String,
    pub updated_at: String,
    pub user: CommentAdminUserRead,
    pub anime_slug: String,
    pub anime_title_tr: String,
    pub episode_number: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentAdminPage {
    pub items: Vec<CommentAdminRead>,
    pub total: i64,
    pub page: i64,
    pub size: i64,
    pub page_size: i64,
    pub pages: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentReportCreate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anime_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_number: Option<f64>,
    pub report_type: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentReportUpdate {
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentReportRead {
    pub id: i64,
    pub anime_id: Option<i64>,
    pub anime_slug: Option<String>,
    pub episode_number: Option<f64>,
    pub report_type: String,
    pub message: String,
    pub page_url: Option<String>,
    pub user_id: Option<i64>,
    pub reporter_username: Option<String>,
    pub reporter_display_name: Option<String>,
    pub reporter_email: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentReportPage {
    pub items: Vec<ContentReportRead>,
    pub total: i64,
    pub page: i64,
    pub size: i64,
    pub page_size: i64,
    pub pages: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordWebhookCreate {
    pub name: String,
    pub url: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_json: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_avatar_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mention_role_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_emoji: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footer_icon_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiscordWebhookUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_json: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_avatar_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mention_role_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_emoji: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footer_icon_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordWebhookRead {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    pub enabled: bool,
    pub event_type: String,
    pub embed_json: Option<Value>,
    pub bot_username: Option<String>,
    pub bot_avatar_url: Option<String>,
    pub mention_role_id: Option<String>,
    pub custom_emoji: Option<String>,
    pub footer_icon_url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub masked_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyCreate {
    pub name: String,
    #[serde(default = "default_admin")]
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyCreateResponse {
    pub id: i64,
    pub name: String,
    pub key_prefix: String,
    pub scope: String,
    pub token: String,
    pub expires_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ApiKeyUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyRead {
    pub id: i64,
    pub name: String,
    pub key_prefix: String,
    pub scope: String,
    pub is_active: bool,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    #[serde(default)]
    pub is_expired: Option<bool>,
}
