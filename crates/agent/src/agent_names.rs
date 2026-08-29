// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.

//! Names for delegated agents, drawn from the people whose work the platform
//! is built on.
//!
//! An orchestrated agent's id defaulted to `task-0`, `task-1`. That is unique
//! and unreadable: with several agents running at once, "task-3 is reading the
//! seals paper" tells a watcher nothing they can hold onto, while "Hume-Rothery
//! is reading the seals paper" is a thing a person remembers between glances.
//! The name IS the identity — the same string the report echoes and the one an
//! interface can group a lane by — so this is a better default, not a decoration
//! layered over one.
//!
//! Surnames only, of scientists in the physical sciences, because that is the
//! domain PRISM works in: Hume-Rothery on alloys, Moissan on fluorine, Wagner
//! on solid-state diffusion, Staudinger on polymers. A run that fans out over a
//! materials question is then narrated by the people who built the field.
//!
//! Selection is DETERMINISTIC — hashed from the task text — so the same task
//! draws the same name on every run. Reproducible transcripts matter more here
//! than novelty, and a test can assert an exact name.

use std::collections::HashSet;

/// Where a scientist worked, as a rough label — used only for weighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Indian,
    German,
    Polish,
    French,
    British,
    American,
    Japanese,
    Chinese,
    Russian,
}

impl Origin {
    /// Relative share of the pool. Indian and German are weighted heaviest at
    /// the owner's request; everything else shares the remainder evenly, so
    /// the whole history of the field still shows up.
    const fn weight(self) -> u32 {
        match self {
            Origin::Indian => 5,
            Origin::German => 4,
            _ => 2,
        }
    }
}

/// The broad area someone worked in, so a task about polymers can be handed to
/// someone who worked on polymers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Materials,
    Chemistry,
    Physics,
    Maths,
    Life,
}

pub struct Scientist {
    pub surname: &'static str,
    pub origin: Origin,
    pub field: Field,
}

const fn s(surname: &'static str, origin: Origin, field: Field) -> Scientist {
    Scientist {
        surname,
        origin,
        field,
    }
}

/// The pool. Surnames of people who are no longer living or whose names are
/// long since attached to the effects themselves (Raman scattering, the
/// Hall-Petch relation, Hume-Rothery rules) — a label pointing at a body of
/// work, never a claim about a person.
pub static SCIENTISTS: &[Scientist] = &[
    // ── Indian ────────────────────────────────────────────────────────
    s("Raman", Origin::Indian, Field::Physics),
    s("Bose", Origin::Indian, Field::Physics),
    s("Saha", Origin::Indian, Field::Physics),
    s("Chandrasekhar", Origin::Indian, Field::Physics),
    s("Bhabha", Origin::Indian, Field::Physics),
    s("Krishnan", Origin::Indian, Field::Physics),
    s("Rao", Origin::Indian, Field::Materials),
    s("Ramanujan", Origin::Indian, Field::Maths),
    s("Khorana", Origin::Indian, Field::Life),
    s("Sarabhai", Origin::Indian, Field::Physics),
    s("Narlikar", Origin::Indian, Field::Physics),
    s("Mashelkar", Origin::Indian, Field::Chemistry),
    s("Seshadri", Origin::Indian, Field::Materials),
    s("Kothari", Origin::Indian, Field::Physics),
    s("Ramachandran", Origin::Indian, Field::Materials),
    s("Baliga", Origin::Indian, Field::Materials),
    s("Chaudhari", Origin::Indian, Field::Materials),
    s("Bhatnagar", Origin::Indian, Field::Chemistry),
    s("Ray", Origin::Indian, Field::Chemistry),
    s("Chatterjee", Origin::Indian, Field::Chemistry),
    s("Sharma", Origin::Indian, Field::Chemistry),
    // ── German ────────────────────────────────────────────────────────
    s("Wagner", Origin::German, Field::Materials),
    s("Schottky", Origin::German, Field::Materials),
    s("Staudinger", Origin::German, Field::Chemistry),
    s("Ziegler", Origin::German, Field::Chemistry),
    s("Goldschmidt", Origin::German, Field::Materials),
    s("Haber", Origin::German, Field::Chemistry),
    s("Bosch", Origin::German, Field::Chemistry),
    s("Nernst", Origin::German, Field::Chemistry),
    s("Liebig", Origin::German, Field::Chemistry),
    s("Bunsen", Origin::German, Field::Chemistry),
    s("Ruska", Origin::German, Field::Materials),
    s("Binnig", Origin::German, Field::Materials),
    s("Roentgen", Origin::German, Field::Physics),
    s("Planck", Origin::German, Field::Physics),
    s("Sommerfeld", Origin::German, Field::Physics),
    s("Moessbauer", Origin::German, Field::Physics),
    s("Diels", Origin::German, Field::Chemistry),
    s("Kroll", Origin::German, Field::Materials),
    s("Ostwald", Origin::German, Field::Chemistry),
    s("Fischer", Origin::German, Field::Chemistry),
    s("Hofmann", Origin::German, Field::Chemistry),
    // ── Polish ────────────────────────────────────────────────────────
    s("Sklodowska", Origin::Polish, Field::Chemistry),
    s("Smoluchowski", Origin::Polish, Field::Physics),
    s("Olszewski", Origin::Polish, Field::Chemistry),
    s("Banach", Origin::Polish, Field::Maths),
    s("Czochralski", Origin::Polish, Field::Materials),
    // ── French ────────────────────────────────────────────────────────
    s("Moissan", Origin::French, Field::Chemistry),
    s("Lavoisier", Origin::French, Field::Chemistry),
    s("Neel", Origin::French, Field::Materials),
    s("Guinier", Origin::French, Field::Materials),
    s("Friedel", Origin::French, Field::Materials),
    s("Becquerel", Origin::French, Field::Physics),
    s("Carnot", Origin::French, Field::Physics),
    // ── British ───────────────────────────────────────────────────────
    s("Hume", Origin::British, Field::Materials), // Hume-Rothery, alloy rules
    s("Bragg", Origin::British, Field::Materials),
    s("Cottrell", Origin::British, Field::Materials),
    s("Petch", Origin::British, Field::Materials),
    s("Mott", Origin::British, Field::Materials),
    s("Faraday", Origin::British, Field::Chemistry),
    s("Davy", Origin::British, Field::Chemistry),
    s("Dalton", Origin::British, Field::Chemistry),
    s("Maxwell", Origin::British, Field::Physics),
    s("Franklin", Origin::British, Field::Life),
    s("Hodgkin", Origin::British, Field::Chemistry),
    // ── American ──────────────────────────────────────────────────────
    s("Gibbs", Origin::American, Field::Materials),
    s("Pauling", Origin::American, Field::Chemistry),
    s("Langmuir", Origin::American, Field::Chemistry),
    s("Seitz", Origin::American, Field::Materials),
    s("Bardeen", Origin::American, Field::Physics),
    s("Shockley", Origin::American, Field::Physics),
    s("Onsager", Origin::American, Field::Chemistry),
    // ── Japanese ──────────────────────────────────────────────────────
    s("Iijima", Origin::Japanese, Field::Materials),
    s("Shirakawa", Origin::Japanese, Field::Chemistry),
    s("Nakamura", Origin::Japanese, Field::Materials),
    s("Akasaki", Origin::Japanese, Field::Materials),
    s("Tanaka", Origin::Japanese, Field::Chemistry),
    s("Yukawa", Origin::Japanese, Field::Physics),
    s("Nagaoka", Origin::Japanese, Field::Physics),
    // ── Chinese ───────────────────────────────────────────────────────
    s("Huang", Origin::Chinese, Field::Materials),
    s("Zhao", Origin::Chinese, Field::Materials),
    s("Yang", Origin::Chinese, Field::Physics),
    s("Wu", Origin::Chinese, Field::Physics),
    s("Tu", Origin::Chinese, Field::Life),
    s("Qian", Origin::Chinese, Field::Physics),
    // ── Russian ───────────────────────────────────────────────────────
    s("Mendeleev", Origin::Russian, Field::Chemistry),
    s("Frenkel", Origin::Russian, Field::Materials),
    s("Ioffe", Origin::Russian, Field::Materials),
    s("Alferov", Origin::Russian, Field::Materials),
    s("Landau", Origin::Russian, Field::Physics),
    s("Kapitsa", Origin::Russian, Field::Physics),
    s("Semenov", Origin::Russian, Field::Chemistry),
    s("Lomonosov", Origin::Russian, Field::Chemistry),
];

/// Who goes first, in order, before anything is hashed.
///
/// Sarabhai founded the Indian space programme and Bhabha its nuclear one;
/// both built the institutions rather than only the results. The owner's call,
/// and a defensible one: the first two lanes of any fan-out are the ones a
/// watcher looks at first, so they get the names worth reading first.
///
/// This deliberately OUTRANKS field matching — a chemistry task can draw
/// Bhabha, a physicist. The cost is one less apt name on the first two lanes;
/// what is bought is a stable, recognisable opening to every run, which is
/// worth more than a marginally better topical fit.
pub static FOUNDERS: &[&str] = &["Sarabhai", "Bhabha"];

/// The field a task is about, from words the task itself uses.
///
/// Keyword matching, not classification: a wrong guess costs a less apt name
/// and nothing else, so this is deliberately the cheapest thing that works.
/// `None` when nothing matches, which draws from the whole pool.
#[must_use]
pub fn field_of(task: &str) -> Option<Field> {
    let lower = task.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| lower.contains(word));

    if has(&[
        "alloy",
        "microstructure",
        "lattice",
        "crystal",
        "grain",
        "phase diagram",
        "sinter",
        "weld",
        "coating",
        "semiconductor",
        "ceramic",
        "composite",
        "elastomer",
        "material",
    ]) {
        return Some(Field::Materials);
    }
    if has(&[
        "synthesis",
        "reaction",
        "catalys",
        "polymer",
        "solvent",
        "ligand",
        "chemical",
        "fluorin",
        "oxidation",
        "monomer",
    ]) {
        return Some(Field::Chemistry);
    }
    if has(&[
        "quantum",
        "spectroscop",
        "photon",
        "magnet",
        "thermodynam",
        "scattering",
        "phonon",
    ]) {
        return Some(Field::Physics);
    }
    if has(&["theorem", "proof", "algebra", "topolog", "manifold"]) {
        return Some(Field::Maths);
    }
    if has(&["protein", "cell", "genome", "enzyme", "toxicit", "clinical"]) {
        return Some(Field::Life);
    }
    None
}

/// Deterministic 64-bit hash — FNV-1a. Stable across runs and platforms, which
/// `DefaultHasher` explicitly does not promise.
fn hash(text: &str) -> u64 {
    let mut value: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        value ^= u64::from(*byte);
        value = value.wrapping_mul(0x0000_0100_0000_01b3);
    }
    value
}

/// Name one agent for `task`, avoiding anything in `taken`.
///
/// Prefers someone who worked in the task's own field, then anyone, weighted
/// by origin. Falls back to `task-<index>` only when every name in the pool is
/// already in use — a fan-out wider than the pool is possible, and silently
/// reusing a name would be worse than an ugly one, because two lanes sharing a
/// label is exactly the confusion the naming exists to prevent.
#[must_use]
pub fn name_for(task: &str, index: usize, taken: &HashSet<String>) -> String {
    // The openers, in order, while any remain.
    if let Some(founder) = FOUNDERS.iter().find(|name| !taken.contains(**name)) {
        return (*founder).to_string();
    }

    let field = field_of(task);
    let seed = hash(task);

    // Weighted candidate order: each scientist gets `weight` tickets, and the
    // hash picks a starting ticket. Probing forward from there keeps selection
    // deterministic while letting a taken name yield to the next candidate.
    let mut tickets: Vec<usize> = Vec::new();
    for (position, scientist) in SCIENTISTS.iter().enumerate() {
        let matches_field = field.is_none_or(|wanted| scientist.field == wanted);
        if !matches_field {
            continue;
        }
        for _ in 0..scientist.origin.weight() {
            tickets.push(position);
        }
    }
    // Nobody in that field: fall back to the whole pool rather than to a
    // number.
    if tickets.is_empty() {
        for (position, scientist) in SCIENTISTS.iter().enumerate() {
            for _ in 0..scientist.origin.weight() {
                tickets.push(position);
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    let start = (seed % tickets.len() as u64) as usize;
    for offset in 0..tickets.len() {
        let candidate = SCIENTISTS[tickets[(start + offset) % tickets.len()]].surname;
        if !taken.contains(candidate) {
            return candidate.to_string();
        }
    }

    // Every name in the pool is spoken for.
    format!("task-{index}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sarabhai then Bhabha open every run, whatever the first tasks are
    /// about — the owner's call, and the first two lanes are the ones anyone
    /// looks at first.
    #[test]
    fn sarabhai_and_bhabha_are_the_first_two_agents() {
        let mut taken: HashSet<String> = HashSet::new();
        let first = name_for("compare alloy microstructure after sintering", 0, &taken);
        assert_eq!(first, "Sarabhai");
        taken.insert(first);

        let second = name_for("optimise the fluorination catalyst", 1, &taken);
        assert_eq!(second, "Bhabha", "even for a chemistry task");
        taken.insert(second);

        // Third onwards is the ordinary field-matched draw again.
        let third = name_for("compare alloy microstructure after sintering", 2, &taken);
        let picked = SCIENTISTS
            .iter()
            .find(|sc| sc.surname == third)
            .expect("from the pool");
        assert_eq!(picked.field, Field::Materials, "back to the field: {third}");
    }

    #[test]
    fn the_same_task_always_draws_the_same_name() {
        let taken = HashSet::new();
        let first = name_for("survey PFAS-free elastomer seals", 0, &taken);
        let again = name_for("survey PFAS-free elastomer seals", 0, &taken);
        assert_eq!(first, again, "transcripts must be reproducible");
        assert!(
            SCIENTISTS.iter().any(|sc| sc.surname == first),
            "a real name from the pool, not a number: {first}"
        );
    }

    /// Two lanes sharing a label is the exact confusion the naming exists to
    /// prevent, so a taken name yields to the next candidate.
    #[test]
    fn no_two_agents_in_a_run_share_a_name() {
        let mut taken: HashSet<String> = HashSet::new();
        for index in 0..40 {
            let name = name_for(&format!("investigate candidate {index}"), index, &taken);
            assert!(taken.insert(name.clone()), "duplicate name: {name}");
        }
    }

    /// A materials question is narrated by people who worked on materials —
    /// once the openers are past. `taken` starts with the founders because
    /// they outrank field matching by design (see `FOUNDERS`), so testing the
    /// field rule means testing the draw that happens after them.
    #[test]
    fn a_task_is_named_from_its_own_field() {
        let taken: HashSet<String> = FOUNDERS.iter().map(|name| (*name).to_string()).collect();
        for task in [
            "compare alloy microstructure after sintering",
            "which ceramic coating resists this elastomer solvent",
        ] {
            let name = name_for(task, 0, &taken);
            let picked = SCIENTISTS
                .iter()
                .find(|sc| sc.surname == name)
                .expect("from the pool");
            assert_eq!(picked.field, Field::Materials, "{task} -> {name}");
        }

        let chemistry = name_for("optimise the fluorination reaction catalyst", 0, &taken);
        let picked = SCIENTISTS
            .iter()
            .find(|sc| sc.surname == chemistry)
            .expect("from the pool");
        assert_eq!(picked.field, Field::Chemistry);
    }

    /// Weighting is the owner's: Indian heaviest, German next. Asserted over
    /// the WHOLE pool rather than on one draw, because a single hash landing
    /// somewhere proves nothing about the distribution.
    #[test]
    fn indian_and_german_names_are_the_most_likely_to_be_drawn() {
        let tickets = |origin: Origin| -> u32 {
            SCIENTISTS
                .iter()
                .filter(|sc| sc.origin == origin)
                .map(|sc| sc.origin.weight())
                .sum()
        };
        let indian = tickets(Origin::Indian);
        let german = tickets(Origin::German);
        for other in [
            Origin::Polish,
            Origin::French,
            Origin::American,
            Origin::Japanese,
            Origin::Chinese,
            Origin::Russian,
        ] {
            assert!(indian > tickets(other), "Indian outweighs {other:?}");
            assert!(german > tickets(other), "German outweighs {other:?}");
        }
        assert!(indian > german, "Indian is the heaviest");
    }

    /// A fan-out wider than the pool must not silently reuse a name.
    #[test]
    fn an_exhausted_pool_falls_back_to_a_number_not_a_repeat() {
        let taken: HashSet<String> = SCIENTISTS.iter().map(|sc| sc.surname.to_string()).collect();
        assert_eq!(name_for("anything at all", 99, &taken), "task-99");
    }
}
