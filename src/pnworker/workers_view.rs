#[derive(Clone, Debug, PartialEq)]
pub struct WorkerJobView {
    pub worker: String,
    pub active: bool,
    pub waiting: bool,
    pub job_id: u64,
    pub organisation: String,
    pub type_label: &'static str,
    pub stage_label: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkerCell {
    pub name: String,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkersModel {
    pub download: Vec<Option<WorkerCell>>,
    pub core: Vec<Option<WorkerCell>>,
    pub upload: Vec<Option<WorkerCell>>,
    pub active: Vec<String>,
    pub waiting: Vec<String>,
    pub queue_len: usize,
}

pub fn build_workers_model(
    views: &[WorkerJobView],
    download_slots: Vec<String>,
    probe_slots: Vec<String>,
    upload_slots: Vec<String>,
) -> WorkersModel {
    let mut download = prefixed_slots("dwl", download_slots);
    let mut core = prefixed_slots("prb", probe_slots);
    core.push("enc-main".to_string());
    let mut upload = prefixed_slots("upl", upload_slots);
    for view in views.iter().filter(|view| view.active) {
        if view.worker.starts_with("dwl-") && !download.iter().any(|slot| slot == &view.worker) {
            download.push(view.worker.clone());
        }
        if view.worker.starts_with("upl-") && !upload.iter().any(|slot| slot == &view.worker) {
            upload.push(view.worker.clone());
        }
        if view.worker.starts_with("prb-") && !core.iter().any(|slot| slot == &view.worker) {
            core.push(view.worker.clone());
        }
    }

    let download_offsets = worker_offsets(download.len());
    let core_offsets = worker_offsets(core.len());
    let upload_offsets = worker_offsets(upload.len());
    let max_offset = download_offsets
        .iter()
        .chain(core_offsets.iter())
        .chain(upload_offsets.iter())
        .map(|offset| offset.abs())
        .max()
        .unwrap_or(2)
        .max(2);
    let height = (max_offset as usize * 2) + 1;
    let center = max_offset;
    let mut rows = vec![(None::<WorkerCell>, None::<WorkerCell>, None::<WorkerCell>); height];

    for (worker, offset) in download.iter().zip(download_offsets) {
        rows[(center + offset) as usize].0 = Some(cell_for_worker(views, worker));
    }
    for (worker, offset) in upload.iter().zip(upload_offsets) {
        rows[(center + offset) as usize].2 = Some(cell_for_worker(views, worker));
    }
    for (worker, offset) in core.iter().zip(core_offsets) {
        rows[(center + offset) as usize].1 = Some(cell_for_worker(views, worker));
    }

    let mut model = WorkersModel {
        download: Vec::new(),
        core: Vec::new(),
        upload: Vec::new(),
        active: active_lines(views, &download, &core, &upload),
        waiting: views
            .iter()
            .filter(|view| view.waiting)
            .map(job_line)
            .collect(),
        queue_len: views.len(),
    };
    for (download, core, upload) in rows
        .into_iter()
        .filter(|row| row.0.is_some() || row.1.is_some() || row.2.is_some())
    {
        model.download.push(download);
        model.core.push(core);
        model.upload.push(upload);
    }
    model
}

pub fn render_workers_columns(model: &WorkersModel) -> (String, String, String) {
    (
        render_column(&model.download),
        render_column(&model.core),
        render_column(&model.upload),
    )
}

pub fn render_detail_lines(lines: &[String]) -> String {
    limit_lines(lines.iter().cloned().collect()).join("\n")
}

pub fn worker_waiting(worker: &str) -> bool {
    matches!(
        worker,
        "dwl-pending"
            | "dwl-cache"
            | "prb-pending"
            | "upl-pending"
            | "enc-forward"
            | "dwl-forward"
            | "upl-forward"
    ) || worker.starts_with("key-")
}

fn prefixed_slots(prefix: &str, slots: Vec<String>) -> Vec<String> {
    slots
        .into_iter()
        .map(|slot| format!("{}-{}", prefix, slot))
        .collect()
}

pub(crate) fn worker_offsets(count: usize) -> Vec<i32> {
    if count == 0 {
        return Vec::new();
    }
    if count % 2 == 0 {
        let start = -((count as i32) - 1);
        (0..count).map(|idx| start + (idx as i32 * 2)).collect()
    } else {
        let half = count as i32 / 2;
        (0..count).map(|idx| (idx as i32 - half) * 2).collect()
    }
}

fn cell_for_worker(views: &[WorkerJobView], worker: &str) -> WorkerCell {
    let active = views
        .iter()
        .any(|view| view.worker == worker && view.active);
    WorkerCell {
        name: worker.to_string(),
        active,
    }
}

fn active_lines(
    views: &[WorkerJobView],
    download: &[String],
    core: &[String],
    upload: &[String],
) -> Vec<String> {
    let mut workers = Vec::new();
    workers.extend(download.iter().cloned());
    workers.extend(core.iter().cloned());
    workers.extend(upload.iter().cloned());

    let mut lines = Vec::new();
    for worker in &workers {
        if let Some(view) = views
            .iter()
            .find(|view| view.worker == *worker && view.active)
        {
            lines.push(active_line(view));
        }
    }
    for view in views
        .iter()
        .filter(|view| view.active && !workers.iter().any(|worker| worker == &view.worker))
    {
        lines.push(active_line(view));
    }
    lines
}

fn active_line(view: &WorkerJobView) -> String {
    format!(
        "{} {} {} {}",
        view.organisation, view.stage_label, view.type_label, view.worker
    )
}

fn job_line(view: &WorkerJobView) -> String {
    format!(
        "{} {} {}",
        view.organisation, view.stage_label, view.type_label
    )
}

fn render_column(cells: &[Option<WorkerCell>]) -> String {
    let lines = cells
        .iter()
        .map(|cell| match cell {
            Some(cell) if cell.active => format!("🟢 {}", cell.name),
            Some(cell) => format!("⚪ {}", cell.name),
            None => "\u{200b}".to_string(),
        })
        .collect();
    limit_lines(lines).join("\n")
}

fn limit_lines(lines: Vec<String>) -> Vec<String> {
    const LIMIT: usize = 1024;
    let mut out = Vec::new();
    let mut used = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        let add = line.len() + if out.is_empty() { 0 } else { 1 };
        if used + add > LIMIT {
            let remaining = lines.len() - idx;
            let tail = format!("...and {} more", remaining);
            if out.is_empty() {
                out.push(tail);
            } else {
                while used + 1 + tail.len() > LIMIT {
                    let Some(removed) = out.pop() else {
                        break;
                    };
                    used -= removed.len();
                    if !out.is_empty() {
                        used -= 1;
                    }
                }
                out.push(tail);
            }
            break;
        }
        used += add;
        out.push(line.clone());
    }
    if out.is_empty() {
        out.push("\u{200b}".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(worker: &str, job_id: u64) -> WorkerJobView {
        WorkerJobView {
            worker: worker.to_string(),
            active: true,
            waiting: false,
            job_id,
            organisation: "Org".to_string(),
            type_label: "encode",
            stage_label: "encoding",
        }
    }

    fn waiting(worker: &str, job_id: u64) -> WorkerJobView {
        WorkerJobView {
            worker: worker.to_string(),
            active: false,
            waiting: worker_waiting(worker),
            job_id,
            organisation: "Org".to_string(),
            type_label: "encode",
            stage_label: "queued",
        }
    }

    #[test]
    fn worker_offsets_match_centered_order() {
        assert_eq!(worker_offsets(0), Vec::<i32>::new());
        assert_eq!(worker_offsets(1), vec![0]);
        assert_eq!(worker_offsets(2), vec![-1, 1]);
        assert_eq!(worker_offsets(3), vec![-2, 0, 2]);
        assert_eq!(worker_offsets(4), vec![-3, -1, 1, 3]);
    }

    #[test]
    fn model_centers_core_and_pads_columns() {
        let views = vec![active("dwl-a", 10), active("enc-main", 20)];
        let model = build_workers_model(
            &views,
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            vec!["p".to_string()],
            vec!["u".to_string()],
        );

        assert_eq!(model.download.len(), model.core.len());
        assert_eq!(model.core.len(), model.upload.len());
        assert_eq!(model.core[1].as_ref().unwrap().name, "prb-p");
        assert_eq!(model.core[3].as_ref().unwrap().name, "enc-main");
        assert_eq!(model.download[2].as_ref().unwrap().name, "dwl-b");
        assert_eq!(model.upload[2].as_ref().unwrap().name, "upl-u");
        assert_eq!(
            model.active,
            vec![
                "Org encoding encode dwl-a",
                "Org encoding encode enc-main"
            ]
        );
    }

    #[test]
    fn model_keeps_dynamic_active_slot_and_waiting_lines() {
        let views = vec![
            active("dwl-extra", 10),
            active("prb-extra", 13),
            waiting("dwl-pending", 11),
            waiting("prb-pending", 14),
            waiting("key-wait", 12),
        ];
        let model = build_workers_model(
            &views,
            vec!["a".to_string()],
            vec!["p".to_string()],
            vec!["u".to_string()],
        );

        assert!(model.download.iter().any(|cell| {
            cell.as_ref()
                .map(|cell| cell.name.as_str() == "dwl-extra" && cell.active)
                .unwrap_or(false)
        }));
        assert!(model.core.iter().any(|cell| {
            cell.as_ref()
                .map(|cell| cell.name.as_str() == "prb-extra" && cell.active)
                .unwrap_or(false)
        }));
        assert_eq!(
            model.waiting,
            vec![
                "Org queued encode",
                "Org queued encode",
                "Org queued encode"
            ]
        );
    }

    #[test]
    fn renderer_marks_active_idle_and_padding() {
        let model = WorkersModel {
            download: vec![
                Some(WorkerCell {
                    name: "dwl-a".to_string(),
                    active: true,
                }),
                None,
                Some(WorkerCell {
                    name: "dwl-b".to_string(),
                    active: false,
                }),
            ],
            core: vec![
                None,
                Some(WorkerCell {
                    name: "prb-hoshi".to_string(),
                    active: false,
                }),
                None,
            ],
            upload: vec![
                None,
                None,
                Some(WorkerCell {
                    name: "upl-u".to_string(),
                    active: true,
                }),
            ],
            active: Vec::new(),
            waiting: Vec::new(),
            queue_len: 0,
        };

        let (download, core, upload) = render_workers_columns(&model);

        assert_eq!(download, "🟢 dwl-a\n\u{200b}\n⚪ dwl-b");
        assert_eq!(core, "\u{200b}\n⚪ prb-hoshi\n\u{200b}");
        assert_eq!(upload, "\u{200b}\n\u{200b}\n🟢 upl-u");
    }

    #[test]
    fn model_centers_three_probe_slots_with_encoder() {
        let views = vec![active("prb-b", 21)];
        let model = build_workers_model(
            &views,
            vec!["d".to_string()],
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            vec!["u".to_string()],
        );

        let core = model
            .core
            .iter()
            .filter_map(|cell| cell.as_ref())
            .map(|cell| (cell.name.as_str(), cell.active))
            .collect::<Vec<_>>();
        assert_eq!(
            core,
            vec![
                ("prb-a", false),
                ("prb-b", true),
                ("prb-c", false),
                ("enc-main", false),
            ]
        );
        assert_eq!(model.download.len(), 5);
        assert_eq!(model.core.len(), 5);
        assert_eq!(model.upload.len(), 5);
    }
}
