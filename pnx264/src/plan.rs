// Append-only natural-IDR plan format. Flushed partial rows can safely drive disposable AOT work,
// while the final encoder requires the completed marker before reusing any chunk. A torn final row
// is ignored when downloading wins the race and kills the planner.

use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoundaryPlan {
    pub idrs: Vec<u64>,
    pub last_planned_pts: Option<u64>,
    pub submitted: u64,
    pub complete: bool,
}

impl BoundaryPlan {
    pub fn read(path: &Path) -> Result<Self, String> {
        let value = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut lines = value.split('\n');
        if lines.next() != Some("PNPLAN1") {
            return Err("unsupported boundary plan".to_string());
        }
        let trailing_torn = !value.ends_with('\n');
        let mut plan = Self::default();
        let mut body = lines.peekable();
        while let Some(line) = body.next() {
            if line.is_empty() {
                continue;
            }
            let is_torn = trailing_torn && body.peek().is_none();
            let parsed = (|| -> Result<(), String> {
                if let Some(value) = line.strip_prefix("idr|") {
                    let pts: u64 = value.parse().map_err(|_| "invalid IDR position")?;
                    if plan.idrs.last().is_some_and(|last| *last >= pts) {
                        return Err("IDR positions are not strictly increasing".to_string());
                    }
                    plan.idrs.push(pts);
                } else if let Some(value) = line.strip_prefix("progress|") {
                    let (pts, submitted) = value.split_once('|').ok_or("invalid plan progress")?;
                    plan.last_planned_pts = Some(pts.parse().map_err(|_| "invalid planned PTS")?);
                    plan.submitted = submitted.parse().map_err(|_| "invalid submitted count")?;
                } else if let Some(value) = line.strip_prefix("complete|") {
                    let (planned, submitted) = value.split_once('|').ok_or("invalid completed plan")?;
                    let planned: u64 = planned.parse().map_err(|_| "invalid planned frame count")?;
                    plan.submitted = submitted.parse().map_err(|_| "invalid submitted count")?;
                    plan.last_planned_pts = planned.checked_sub(1);
                    plan.complete = true;
                } else {
                    return Err("unknown boundary plan row".to_string());
                }
                Ok(())
            })();
            if let Err(error) = parsed {
                if is_torn {
                    break;
                }
                return Err(error.to_string());
            }
        }
        if plan.idrs.first().copied().is_some_and(|first| first != 0) {
            return Err("boundary plan does not begin with the initial IDR".to_string());
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: &str) -> Result<BoundaryPlan, String> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("pnx264-plan-{}-{nonce}", std::process::id()));
        std::fs::write(&path, value).unwrap();
        let result = BoundaryPlan::read(&path);
        std::fs::remove_file(path).ok();
        result
    }

    #[test]
    fn partial_plan_keeps_only_complete_flushed_rows() {
        let plan = parse("PNPLAN1\nidr|0\nidr|250\nprogress|430|500\nidr|").unwrap();
        assert_eq!(plan.idrs, vec![0, 250]);
        assert_eq!(plan.last_planned_pts, Some(430));
        assert_eq!(plan.submitted, 500);
        assert!(!plan.complete);
    }

    #[test]
    fn rejects_plain_or_unsorted_cut_points() {
        assert!(parse("PNPLAN1\nidr|0\nidr|250\nidr|200\n").is_err());
        assert!(parse("PNPLAN1\nidr|0\ni|250\n").is_err());
    }
}
