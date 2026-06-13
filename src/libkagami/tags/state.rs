use std::mem::discriminant;
use crate::libkagami::complex::overrides::ASSOverride;
use crate::libkagami::tags::transform::transform_inner_tags;

pub fn same_override_kind(a: &ASSOverride, b: &ASSOverride) -> bool {
    matches!((a, b),
        (ASSOverride::A(_), ASSOverride::An(_))
        | (ASSOverride::An(_), ASSOverride::A(_))
    ) || discriminant(a) == discriminant(b)
}

pub fn already_active(current: &[ASSOverride], candidate: &ASSOverride) -> bool {
    if transform_inner_tags(candidate).is_some() {
        return false;
    }
    current
        .iter()
        .any(|c| same_override_kind(c, candidate) && c == candidate)
}

pub fn upsert_override(current: &mut Vec<ASSOverride>, new: ASSOverride) {
    for existing in current.iter_mut() {
        if same_override_kind(existing, &new) {
            *existing = new;
            return;
        }
    }
    current.push(new);
}

pub fn is_first_wins(ov: &ASSOverride) -> bool {
    matches!(ov,
        ASSOverride::Pos(_, _)
        | ASSOverride::A(_)
        | ASSOverride::An(_)
        | ASSOverride::MoveI(_, _, _, _)
        | ASSOverride::MoveII(_, _, _, _, _, _)
        | ASSOverride::Org(_, _)
    )
}

pub fn is_repeatable_effect(ov: &ASSOverride) -> bool {
    matches!(ov,
        ASSOverride::K(_)
        | ASSOverride::Kt(_)
        | ASSOverride::KSweep(_)
        | ASSOverride::Kf(_)
        | ASSOverride::Ko(_)
    )
}
