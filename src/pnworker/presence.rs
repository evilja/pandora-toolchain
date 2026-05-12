use serenity::{
    all::{ActivityData, Context, OnlineStatus},
};

pub async fn change_presence_job(gateway: &Context, data: (Option<usize>, usize)) {
    if data.1 == 0 {
        gateway.set_presence(Some(ActivityData::custom("No jobs in queue.")), OnlineStatus::Online);
        return;
    }
    if let Some(idx) = data.0 {
        gateway.set_presence(Some(ActivityData::custom(format!("Encoding no. {} in total {} jobs", idx.saturating_add(1), data.1))), OnlineStatus::DoNotDisturb);
    } else {
        gateway.set_presence(Some(ActivityData::custom(format!("Total {} jobs, currently not encoding.", data.1))), OnlineStatus::DoNotDisturb);
    }
}
