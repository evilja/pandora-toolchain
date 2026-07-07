use reqwest::{Client, Method};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::time::Duration;

use super::error::{AkiraError, AkiraResult};
use super::queries::*;
use super::types::*;

#[derive(Clone)]
pub struct AkiraClient {
    base_url: String,
    token: Option<String>,
    client: Client,
}

impl AkiraClient {
    pub fn new(base_url: impl Into<String>) -> AkiraResult<Self> {
        Self::with_token(base_url, Option::<String>::None)
    }

    pub fn with_bearer(base_url: impl Into<String>, token: impl Into<String>) -> AkiraResult<Self> {
        Self::with_token(base_url, Some(token.into()))
    }

    pub fn with_token(base_url: impl Into<String>, token: Option<String>) -> AkiraResult<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent("pandora-toolchain hyperkira")
            .build()?;
        Ok(Self {
            base_url: normalize_base_url(base_url.into()),
            token,
            client,
        })
    }

    pub fn set_bearer(&mut self, token: impl Into<String>) {
        self.token = Some(token.into());
    }

    pub fn clear_bearer(&mut self) {
        self.token = None;
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let req = self.client.request(method, self.url(path));
        match &self.token {
            Some(token) if !token.is_empty() => req.bearer_auth(token),
            _ => req,
        }
    }

    async fn send<T: DeserializeOwned>(&self, req: reqwest::RequestBuilder) -> AkiraResult<T> {
        let resp = req.send().await?;
        decode_response(resp).await
    }

    async fn send_empty(&self, req: reqwest::RequestBuilder) -> AkiraResult<()> {
        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status.is_success() {
            Ok(())
        } else {
            Err(AkiraError::Api { status, body })
        }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> AkiraResult<T> {
        self.send(self.request(Method::GET, path)).await
    }

    async fn get_query<T: DeserializeOwned, Q: Serialize + ?Sized>(
        &self,
        path: &str,
        query: &Q,
    ) -> AkiraResult<T> {
        self.send(self.request(Method::GET, path).query(query))
            .await
    }

    async fn post<T: DeserializeOwned, P: Serialize + ?Sized>(
        &self,
        path: &str,
        payload: &P,
    ) -> AkiraResult<T> {
        self.send(self.request(Method::POST, path).json(payload))
            .await
    }

    async fn patch<T: DeserializeOwned, P: Serialize + ?Sized>(
        &self,
        path: &str,
        payload: &P,
    ) -> AkiraResult<T> {
        self.send(self.request(Method::PATCH, path).json(payload))
            .await
    }

    async fn put<T: DeserializeOwned, P: Serialize + ?Sized>(
        &self,
        path: &str,
        payload: &P,
    ) -> AkiraResult<T> {
        self.send(self.request(Method::PUT, path).json(payload))
            .await
    }

    async fn delete_empty(&self, path: &str) -> AkiraResult<()> {
        self.send_empty(self.request(Method::DELETE, path)).await
    }

    async fn delete_json_empty<P: Serialize + ?Sized>(
        &self,
        path: &str,
        payload: &P,
    ) -> AkiraResult<()> {
        self.send_empty(self.request(Method::DELETE, path).json(payload))
            .await
    }

    async fn delete_json<T: DeserializeOwned, P: Serialize + ?Sized>(
        &self,
        path: &str,
        payload: &P,
    ) -> AkiraResult<T> {
        self.send(self.request(Method::DELETE, path).json(payload))
            .await
    }

    pub async fn health(&self) -> AkiraResult<HealthRead> {
        self.get("/health").await
    }

    pub async fn stats(&self) -> AkiraResult<AnimeStats> {
        self.get("/stats").await
    }

    pub async fn public_settings(&self) -> AkiraResult<PublicSettingsRead> {
        self.get("/settings/public").await
    }

    pub async fn recruitment_settings(&self) -> AkiraResult<RecruitmentSettings> {
        self.get("/settings/recruitment").await
    }

    pub async fn staff_roles(&self) -> AkiraResult<StaffRolesSettings> {
        self.get("/settings/staff-roles").await
    }

    pub async fn login(&self, payload: &LoginRequest) -> AkiraResult<TokenResponse> {
        self.post("/auth/login", payload).await
    }

    pub async fn register(&self, payload: &RegisterRequest) -> AkiraResult<RegistrationResponse> {
        self.post("/auth/register", payload).await
    }

    pub async fn me(&self) -> AkiraResult<UserRead> {
        self.get("/auth/me").await
    }

    pub async fn update_me(&self, payload: &UserMeUpdate) -> AkiraResult<UserRead> {
        self.patch("/auth/me", payload).await
    }

    pub async fn upload_avatar(
        &self,
        filename: impl Into<String>,
        bytes: Vec<u8>,
    ) -> AkiraResult<UserRead> {
        let part = reqwest::multipart::Part::bytes(bytes).file_name(filename.into());
        let form = reqwest::multipart::Form::new().part("file", part);
        self.send(
            self.request(Method::POST, "/auth/me/avatar")
                .multipart(form),
        )
        .await
    }

    pub async fn delete_avatar(&self) -> AkiraResult<UserRead> {
        self.send(self.request(Method::DELETE, "/auth/me/avatar"))
            .await
    }

    pub async fn list_crew_members(&self) -> AkiraResult<Vec<CrewMemberRead>> {
        self.get("/crew/").await
    }

    pub async fn get_crew_member(&self, crew_id: i64) -> AkiraResult<CrewMemberRead> {
        self.get(&format!("/crew/{}", crew_id)).await
    }

    pub async fn list_animes(&self, query: &AnimeListQuery) -> AkiraResult<AnimePage> {
        self.get_query("/animes", query).await
    }

    pub async fn anime_genres(&self) -> AkiraResult<Vec<String>> {
        self.get("/animes/genres").await
    }

    pub async fn anime_formats(&self) -> AkiraResult<Vec<String>> {
        self.get("/animes/formats").await
    }

    pub async fn anime_statuses(&self) -> AkiraResult<Vec<String>> {
        self.get("/animes/statuses").await
    }

    pub async fn featured_animes(&self, limit: Option<i64>) -> AkiraResult<Vec<AnimeListItem>> {
        self.get_query("/animes/featured", &LimitQuery { limit })
            .await
    }

    pub async fn rate_anime(
        &self,
        slug: &str,
        payload: &AnimeRatingWrite,
    ) -> AkiraResult<AnimeRatingRead> {
        self.post(&format!("/animes/{}/rate", slug), payload).await
    }

    pub async fn delete_anime_rating(&self, slug: &str) -> AkiraResult<AnimeRatingRead> {
        self.send(self.request(Method::DELETE, &format!("/animes/{}/rate", slug)))
            .await
    }

    pub async fn anime_rating(&self, slug: &str) -> AkiraResult<AnimeRatingRead> {
        self.get(&format!("/animes/{}/rating", slug)).await
    }

    pub async fn anime(&self, slug: &str) -> AkiraResult<AnimeDetail> {
        self.get(&format!("/animes/{}", slug)).await
    }

    pub async fn anime_episodes(
        &self,
        slug: &str,
        query: &EpisodeListQuery,
    ) -> AkiraResult<EpisodePage> {
        self.get_query(&format!("/animes/{}/episodes", slug), query)
            .await
    }

    pub async fn announcements(&self, all_pages: bool) -> AkiraResult<Vec<AnnouncementRead>> {
        self.get_query("/announcements", &AllPagesQuery { all_pages })
            .await
    }

    pub async fn dismiss_announcement(&self, id: i64) -> AkiraResult<()> {
        self.send_empty(self.request(Method::POST, &format!("/announcements/{}/dismiss", id)))
            .await
    }

    pub async fn create_content_report(
        &self,
        payload: &ContentReportCreate,
    ) -> AkiraResult<ContentReportRead> {
        self.post("/content-reports", payload).await
    }

    pub async fn list_comments(
        &self,
        slug: &str,
        query: &CommentListQuery,
    ) -> AkiraResult<CommentPage> {
        self.get_query(&format!("/animes/{}/comments", slug), query)
            .await
    }

    pub async fn create_comment(
        &self,
        slug: &str,
        payload: &CommentCreate,
    ) -> AkiraResult<CommentRead> {
        self.post(&format!("/animes/{}/comments", slug), payload)
            .await
    }

    pub async fn update_comment(
        &self,
        comment_id: i64,
        payload: &CommentUpdate,
    ) -> AkiraResult<CommentRead> {
        self.patch(&format!("/comments/{}", comment_id), payload)
            .await
    }

    pub async fn delete_comment(&self, comment_id: i64) -> AkiraResult<()> {
        self.delete_empty(&format!("/comments/{}", comment_id))
            .await
    }

    pub async fn vote_comment(
        &self,
        comment_id: i64,
        payload: &CommentVoteCreate,
    ) -> AkiraResult<CommentRead> {
        self.post(&format!("/comments/{}/vote", comment_id), payload)
            .await
    }

    pub async fn watch_history(&self, limit: Option<i64>) -> AkiraResult<Vec<WatchHistoryRead>> {
        self.get_query("/user/history", &LimitQuery { limit }).await
    }

    pub async fn upsert_watch_history(
        &self,
        payload: &WatchHistoryUpsert,
    ) -> AkiraResult<WatchHistoryRead> {
        self.post("/user/history", payload).await
    }

    pub async fn clear_watch_history(&self) -> AkiraResult<()> {
        self.delete_empty("/user/history").await
    }

    pub async fn delete_watch_history_episode(&self, episode_id: i64) -> AkiraResult<()> {
        self.delete_empty(&format!("/user/history/{}", episode_id))
            .await
    }

    pub async fn delete_watch_history_series(&self, anime_slug: &str) -> AkiraResult<()> {
        self.delete_empty(&format!("/user/history/series/{}", anime_slug))
            .await
    }

    pub async fn watchlist(&self, status: Option<&str>) -> AkiraResult<Vec<WatchlistRead>> {
        self.get_query("/user/watchlist", &StatusQuery { status })
            .await
    }

    pub async fn add_watchlist(&self, payload: &WatchlistCreate) -> AkiraResult<WatchlistRead> {
        self.post("/user/watchlist", payload).await
    }

    pub async fn update_watchlist(
        &self,
        anime_id: i64,
        payload: &WatchlistUpdate,
    ) -> AkiraResult<WatchlistRead> {
        self.patch(&format!("/user/watchlist/{}", anime_id), payload)
            .await
    }

    pub async fn remove_watchlist(&self, anime_id: i64) -> AkiraResult<()> {
        self.delete_empty(&format!("/user/watchlist/{}", anime_id))
            .await
    }

    pub async fn continue_watching(
        &self,
        limit: Option<i64>,
    ) -> AkiraResult<Vec<ContinueWatchingItem>> {
        self.get_query("/user/continue-watching", &LimitQuery { limit })
            .await
    }

    pub async fn public_user_history(
        &self,
        username: &str,
        limit: Option<i64>,
    ) -> AkiraResult<Vec<WatchHistoryRead>> {
        self.get_query(
            &format!("/user/{}/history", username),
            &LimitQuery { limit },
        )
        .await
    }

    pub async fn public_user_watchlist(
        &self,
        username: &str,
        query: &PublicWatchlistQuery<'_>,
    ) -> AkiraResult<WatchlistPage> {
        self.get_query(&format!("/user/{}/watchlist", username), query)
            .await
    }

    pub async fn public_user_profile(&self, username: &str) -> AkiraResult<UserPublicRead> {
        self.get(&format!("/user/{}", username)).await
    }

    pub async fn update_site_settings(
        &self,
        payload: &SiteSettingsUpdate,
    ) -> AkiraResult<PublicSettingsRead> {
        self.patch("/admin/settings", payload).await
    }

    pub async fn update_comments_settings(
        &self,
        payload: &CommentsSettingsUpdate,
    ) -> AkiraResult<CommentsSettingsRead> {
        self.patch("/admin/settings/comments", payload).await
    }

    pub async fn update_recruitment_settings(
        &self,
        payload: &RecruitmentSettings,
    ) -> AkiraResult<RecruitmentSettings> {
        self.patch("/admin/settings/recruitment", payload).await
    }

    pub async fn update_staff_roles(
        &self,
        payload: &StaffRolesSettings,
    ) -> AkiraResult<StaffRolesSettings> {
        self.patch("/admin/settings/staff-roles", payload).await
    }

    pub async fn admin_users(&self, query: &UserListQuery<'_>) -> AkiraResult<UserPage> {
        self.get_query("/admin/users", query).await
    }

    pub async fn pending_users(&self, query: &PageQuery) -> AkiraResult<UserPage> {
        self.get_query("/admin/users/pending", query).await
    }

    pub async fn create_user(&self, payload: &UserCreate) -> AkiraResult<UserRead> {
        self.post("/admin/users", payload).await
    }

    pub async fn update_user(&self, user_id: i64, payload: &UserUpdate) -> AkiraResult<UserRead> {
        self.patch(&format!("/admin/users/{}", user_id), payload)
            .await
    }

    pub async fn update_user_by_username(
        &self,
        username: &str,
        payload: &UserUpdate,
    ) -> AkiraResult<UserRead> {
        self.patch(&format!("/admin/users/by-username/{}", username), payload)
            .await
    }

    pub async fn delete_user(&self, user_id: i64) -> AkiraResult<()> {
        self.delete_empty(&format!("/admin/users/{}", user_id))
            .await
    }

    pub async fn invite_codes(
        &self,
        query: &InviteCodeListQuery<'_>,
    ) -> AkiraResult<InviteCodePage> {
        self.get_query("/admin/invite-codes", query).await
    }

    pub async fn create_invite_code(
        &self,
        payload: &InviteCodeCreate,
    ) -> AkiraResult<Vec<InviteCodeRead>> {
        self.post("/admin/invite-codes", payload).await
    }

    pub async fn update_invite_code(
        &self,
        invite_code_id: i64,
        payload: &InviteCodeUpdate,
    ) -> AkiraResult<InviteCodeRead> {
        self.patch(&format!("/admin/invite-codes/{}", invite_code_id), payload)
            .await
    }

    pub async fn invite_code_users(&self, invite_code_id: i64) -> AkiraResult<Vec<UserRead>> {
        self.get(&format!("/admin/invite-codes/{}/users", invite_code_id))
            .await
    }

    pub async fn delete_invite_codes_batch(
        &self,
        payload: &InviteCodeBatchDelete,
    ) -> AkiraResult<InviteCodeBatchDeleteResponse> {
        self.delete_json("/admin/invite-codes/batch", payload).await
    }

    pub async fn delete_invite_code(&self, invite_code_id: i64) -> AkiraResult<()> {
        self.delete_empty(&format!("/admin/invite-codes/{}", invite_code_id))
            .await
    }

    pub async fn admin_animes(&self, query: &AdminAnimeListQuery<'_>) -> AkiraResult<AnimePage> {
        self.get_query("/admin/animes", query).await
    }

    pub async fn create_anime(&self, payload: &AnimeCreate) -> AkiraResult<AnimeDetail> {
        self.post("/admin/animes", payload).await
    }

    pub async fn resolve_anime(&self, query: &AnimeResolveQuery) -> AkiraResult<AnimeResolveRead> {
        self.get_query("/admin/animes/resolve", query).await
    }

    pub async fn resolve_anime_by_mal_id(&self, mal_id: i64) -> AkiraResult<AnimeResolveRead> {
        self.resolve_anime(&AnimeResolveQuery {
            mal_id: Some(mal_id),
            anilist_id: None,
        })
        .await
    }

    pub async fn resolve_anime_by_anilist_id(&self, anilist_id: i64) -> AkiraResult<AnimeResolveRead> {
        self.resolve_anime(&AnimeResolveQuery {
            mal_id: None,
            anilist_id: Some(anilist_id),
        })
        .await
    }

    pub async fn import_anime(&self, payload: &AnimeImportRequest) -> AkiraResult<AnimeDetail> {
        self.post("/admin/animes/import", payload).await
    }

    pub async fn update_anime(
        &self,
        slug: &str,
        payload: &AnimeUpdate,
    ) -> AkiraResult<AnimeDetail> {
        self.patch(&format!("/admin/animes/{}", slug), payload)
            .await
    }

    pub async fn reorder_featured_animes(
        &self,
        payload: &FeaturedOrderUpdate,
    ) -> AkiraResult<Vec<AnimeListItem>> {
        self.put("/admin/animes/featured-order", payload).await
    }

    pub async fn replace_anime_links(
        &self,
        slug: &str,
        payload: &[ExternalLinkWrite],
    ) -> AkiraResult<AnimeDetail> {
        self.put(&format!("/admin/animes/{}/links", slug), payload)
            .await
    }

    pub async fn replace_platform_links(
        &self,
        slug: &str,
        payload: &PlatformLinksWrite,
    ) -> AkiraResult<AnimeDetail> {
        self.put(&format!("/admin/animes/{}/platform-links", slug), payload)
            .await
    }

    pub async fn create_episode(
        &self,
        slug: &str,
        payload: &EpisodeCreate,
    ) -> AkiraResult<EpisodeWriteResponse> {
        self.post(&format!("/admin/animes/{}/episodes", slug), payload)
            .await
    }

    pub async fn update_episode_count(
        &self,
        slug: &str,
        payload: &EpisodeCountUpdate,
    ) -> AkiraResult<EpisodeWriteResponse> {
        self.patch(&format!("/admin/animes/{}/episode-count", slug), payload)
            .await
    }

    pub async fn replace_episode_links(
        &self,
        slug: &str,
        episode_number: f64,
        payload: &[EpisodeLinkWrite],
    ) -> AkiraResult<EpisodeRead> {
        self.put(
            &format!("/admin/animes/{}/episodes/{}/links", slug, episode_number),
            payload,
        )
        .await
    }

    pub async fn update_episode(
        &self,
        slug: &str,
        episode_number: f64,
        payload: &EpisodeUpdate,
    ) -> AkiraResult<EpisodeRead> {
        self.patch(
            &format!("/admin/animes/{}/episodes/{}", slug, episode_number),
            payload,
        )
        .await
    }

    pub async fn replace_anime_staff(
        &self,
        slug: &str,
        payload: &[EpisodeStaffCreditWrite],
    ) -> AkiraResult<AnimeDetail> {
        self.put(&format!("/admin/animes/{}/staff", slug), payload)
            .await
    }

    pub async fn replace_episode_staff(
        &self,
        slug: &str,
        episode_number: f64,
        payload: &[EpisodeStaffCreditWrite],
    ) -> AkiraResult<EpisodeRead> {
        self.put(
            &format!("/admin/animes/{}/episodes/{}/staff", slug, episode_number),
            payload,
        )
        .await
    }

    pub async fn create_episodes_batch(
        &self,
        slug: &str,
        payload: &BatchEpisodeAdd,
    ) -> AkiraResult<Vec<EpisodeWriteResponse>> {
        self.post(&format!("/admin/animes/{}/episodes/batch", slug), payload)
            .await
    }

    pub async fn delete_episodes_batch(
        &self,
        slug: &str,
        payload: &BatchEpisodeDelete,
    ) -> AkiraResult<()> {
        self.delete_json_empty(&format!("/admin/animes/{}/episodes/batch", slug), payload)
            .await
    }

    pub async fn delete_episode(&self, slug: &str, episode_number: f64) -> AkiraResult<()> {
        self.delete_empty(&format!(
            "/admin/animes/{}/episodes/{}",
            slug, episode_number
        ))
        .await
    }

    pub async fn announce_episode(
        &self,
        slug: &str,
        episode_number: f64,
        payload: &AnnounceRequest,
    ) -> AkiraResult<ReleaseEventRead> {
        self.post(
            &format!(
                "/admin/animes/{}/episodes/{}/announce",
                slug, episode_number
            ),
            payload,
        )
        .await
    }

    pub async fn announce_anime(
        &self,
        slug: &str,
        payload: &AnnounceRequest,
    ) -> AkiraResult<AnimeEventRead> {
        self.post(&format!("/admin/animes/{}/announce", slug), payload)
            .await
    }

    pub async fn delete_animes_batch(&self, payload: &BatchAnimeDelete) -> AkiraResult<()> {
        self.delete_json_empty("/admin/animes/batch", payload).await
    }

    pub async fn delete_anime(&self, slug: &str) -> AkiraResult<()> {
        self.delete_empty(&format!("/admin/animes/{}", slug)).await
    }

    pub async fn fetch_anilist_for_anime(&self, slug: &str, id: i64) -> AkiraResult<AnimeDetail> {
        self.send(
            self.request(
                Method::POST,
                &format!("/admin/animes/{}/fetch-anilist", slug),
            )
            .query(&IdQuery { id: Some(id) }),
        )
        .await
    }

    pub async fn crew_stats(&self) -> AkiraResult<Vec<CrewStatsRead>> {
        self.get("/admin/crew/stats").await
    }

    pub async fn admin_crew_members(
        &self,
        query: &LimitOffsetQuery,
    ) -> AkiraResult<Vec<CrewMemberRead>> {
        self.get_query("/admin/crew", query).await
    }

    pub async fn create_crew_member(
        &self,
        payload: &CrewMemberCreate,
    ) -> AkiraResult<CrewMemberRead> {
        self.post("/admin/crew", payload).await
    }

    pub async fn update_crew_member(
        &self,
        crew_id: i64,
        payload: &CrewMemberUpdate,
    ) -> AkiraResult<CrewMemberRead> {
        self.put(&format!("/admin/crew/{}", crew_id), payload).await
    }

    pub async fn delete_crew_member(&self, crew_id: i64) -> AkiraResult<()> {
        self.delete_empty(&format!("/admin/crew/{}", crew_id)).await
    }

    pub async fn admin_announcements(
        &self,
        query: &LimitOffsetQuery,
    ) -> AkiraResult<Vec<AnnouncementRead>> {
        self.get_query("/admin/announcements", query).await
    }

    pub async fn create_announcement(
        &self,
        payload: &AnnouncementCreate,
    ) -> AkiraResult<AnnouncementRead> {
        self.post("/admin/announcements", payload).await
    }

    pub async fn update_announcement(
        &self,
        id: i64,
        payload: &AnnouncementUpdate,
    ) -> AkiraResult<AnnouncementRead> {
        self.patch(&format!("/admin/announcements/{}", id), payload)
            .await
    }

    pub async fn delete_announcement(&self, id: i64) -> AkiraResult<()> {
        self.delete_empty(&format!("/admin/announcements/{}", id))
            .await
    }

    pub async fn cloudflare_analytics(&self) -> AkiraResult<Value> {
        self.get("/admin/analytics/cloudflare").await
    }

    pub async fn admin_comments(
        &self,
        query: &AdminCommentListQuery<'_>,
    ) -> AkiraResult<CommentAdminPage> {
        self.get_query("/admin/comments", query).await
    }

    pub async fn moderate_comment(
        &self,
        comment_id: i64,
        payload: &CommentModerationUpdate,
    ) -> AkiraResult<CommentRead> {
        self.patch(&format!("/admin/comments/{}", comment_id), payload)
            .await
    }

    pub async fn content_reports(
        &self,
        query: &ContentReportListQuery<'_>,
    ) -> AkiraResult<ContentReportPage> {
        self.get_query("/admin/content-reports", query).await
    }

    pub async fn update_content_report(
        &self,
        report_id: i64,
        payload: &ContentReportUpdate,
    ) -> AkiraResult<ContentReportRead> {
        self.patch(&format!("/admin/content-reports/{}", report_id), payload)
            .await
    }

    pub async fn delete_content_report(&self, report_id: i64) -> AkiraResult<()> {
        self.delete_empty(&format!("/admin/content-reports/{}", report_id))
            .await
    }

    pub async fn webhooks(&self, query: &LimitOffsetQuery) -> AkiraResult<Vec<DiscordWebhookRead>> {
        self.get_query("/admin/webhooks", query).await
    }

    pub async fn create_webhook(
        &self,
        payload: &DiscordWebhookCreate,
    ) -> AkiraResult<DiscordWebhookRead> {
        self.post("/admin/webhooks", payload).await
    }

    pub async fn update_webhook(
        &self,
        webhook_id: i64,
        payload: &DiscordWebhookUpdate,
    ) -> AkiraResult<DiscordWebhookRead> {
        self.patch(&format!("/admin/webhooks/{}", webhook_id), payload)
            .await
    }

    pub async fn delete_webhook(&self, webhook_id: i64) -> AkiraResult<()> {
        self.delete_empty(&format!("/admin/webhooks/{}", webhook_id))
            .await
    }

    pub async fn test_webhook(&self, webhook_id: i64) -> AkiraResult<Value> {
        self.send(self.request(
            Method::POST,
            &format!("/admin/webhooks/{}/test", webhook_id),
        ))
        .await
    }

    pub async fn api_keys(&self, query: &LimitOffsetQuery) -> AkiraResult<Vec<ApiKeyRead>> {
        self.get_query("/admin/api-keys", query).await
    }

    pub async fn create_api_key(
        &self,
        payload: &ApiKeyCreate,
    ) -> AkiraResult<ApiKeyCreateResponse> {
        self.post("/admin/api-keys", payload).await
    }

    pub async fn update_api_key(
        &self,
        key_id: i64,
        payload: &ApiKeyUpdate,
    ) -> AkiraResult<ApiKeyRead> {
        self.patch(&format!("/admin/api-keys/{}", key_id), payload)
            .await
    }

    pub async fn delete_api_key(&self, key_id: i64) -> AkiraResult<()> {
        self.delete_empty(&format!("/admin/api-keys/{}", key_id))
            .await
    }
}

fn normalize_base_url(mut base_url: String) -> String {
    while base_url.ends_with('/') {
        base_url.pop();
    }
    if !base_url.ends_with("/api") {
        base_url.push_str("/api");
    }
    base_url
}

async fn decode_response<T: DeserializeOwned>(resp: reqwest::Response) -> AkiraResult<T> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AkiraError::Api { status, body });
    }
    serde_json::from_str(&body).map_err(|source| AkiraError::Decode { source, body })
}
