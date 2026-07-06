use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct LimitQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct LimitOffsetQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PageQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct AnimeListQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crew: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct EpisodeListQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AllPagesQuery {
    pub(crate) all_pages: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StatusQuery<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct IdQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mal_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PublicWatchlistQuery<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'a str>,
    #[serde(flatten)]
    pub page: PageQuery,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UserListQuery<'a> {
    #[serde(flatten)]
    pub page: PageQuery,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct InviteCodeListQuery<'a> {
    #[serde(flatten)]
    pub page: PageQuery,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct AdminAnimeListQuery<'a> {
    #[serde(flatten)]
    pub page: PageQuery,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct AdminCommentListQuery<'a> {
    #[serde(flatten)]
    pub page: PageQuery,
    #[serde(skip_serializing_if = "Option::is_none", rename = "type")]
    pub kind: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct CommentListQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_number: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ContentReportListQuery<'a> {
    #[serde(flatten)]
    pub page: PageQuery,
    #[serde(skip_serializing_if = "Option::is_none", rename = "status")]
    pub status_filter: Option<&'a str>,
}
