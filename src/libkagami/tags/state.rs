use std::mem::discriminant;
use crate::libkagami::complex::overrides::ASSOverride;
use crate::libkagami::tags::transform::transform_inner_tags;

pub fn already_active(current: &[ASSOverride], candidate: &ASSOverride) -> bool {
    if transform_inner_tags(candidate).is_some() {
        return false;
    }
    current
        .iter()
        .any(|c| discriminant(c) == discriminant(candidate) && c == candidate)
}

pub fn upsert_override(current: &mut Vec<ASSOverride>, new: ASSOverride) {
    for existing in current.iter_mut() {
        if discriminant(existing) == discriminant(&new) {
            *existing = new;
            return;
        }
    }
    current.push(new);
}

pub fn is_first_wins(ov: &ASSOverride) -> bool {
    matches!(ov,
        ASSOverride::Pos(_, _)
        | ASSOverride::An(_)
        | ASSOverride::MoveI(_, _, _, _)
        | ASSOverride::MoveII(_, _, _, _, _, _)
        | ASSOverride::Org(_, _)
    )
}
