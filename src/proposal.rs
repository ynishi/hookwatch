//! Proposals: what Jig Smith (an AI) suggests from frictions, checked by
//! gates before a human sees it. The contract: docs/jig-smith.md.
//! Pure: the caller runs the cases and the replay and hands in the results.

use crate::jig::{Action, Jig};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How long the human meant the correction to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// "From now on": becomes a jig or a rule.
    Lasting,
    /// Only for the task at hand: never a lasting jig or rule.
    ThisTask,
    /// Meaning or scope unclear: not acted on (TellOnce: inactive).
    Ambiguous,
}

/// What the proposal turns the friction into.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Candidate {
    /// A deterministic jig (regex or command), entered in Shadow.
    Jig { jig: Jig },
    /// Needs semantic judgement: waits for the L2 Judge, nothing installed.
    HookL2Pending { question: String },
    /// A written rule added to one place (a skill, CLAUDE.md, a doc).
    Rule { file: String, text: String },
    /// Nothing to do, with why.
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expect {
    Hit,
    Pass,
}

/// A validation case: one tool call and whether the jig must hit it
/// (TellOnce: compliance / violation / non-applicability cases).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Case {
    pub tool_name: String,
    pub tool_input: Value,
    pub expect: Expect,
    /// Taken from a friction episode (counts toward recall).
    #[serde(default)]
    pub from_episode: bool,
    #[serde(default)]
    pub note: String,
}

/// What Jig Smith writes. One JSON object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    /// The episodes it came from, as `hookwatch smith-input` gave them
    /// (kind and ts_ms at least).
    pub episodes: Vec<Value>,
    /// What the human wanted, in Smith's words.
    pub intent: String,
    pub scope: Scope,
    /// Why this outlet and not the others.
    pub reason: String,
    pub candidate: Candidate,
    #[serde(default)]
    pub cases: Vec<Case>,
    /// When this repeats an earlier proposal: what changed since.
    #[serde(default)]
    pub changed_since: Option<String>,
    /// The id of a jig in effect that this jig supersedes. On accept, that
    /// jig is retired and this one enters in shadow.
    #[serde(default)]
    pub replaces: Option<String>,
    /// The scripts of a command jig (file name → text), taken by `propose`
    /// when the gate ran them; `accept` writes these, so a script changed
    /// after the gate is not what goes in effect. Smith does not write this.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub scripts: std::collections::BTreeMap<String, String>,
}

/// The shape errors a draft can have. Checked before anything runs.
pub fn check_shape(d: &Draft) -> Vec<String> {
    let mut errs = vec![];
    if d.episodes.is_empty() {
        errs.push("no episodes: a proposal starts from a friction".into());
    }
    if d.intent.trim().is_empty() || d.reason.trim().is_empty() {
        errs.push("intent and reason must be written".into());
    }
    let acts = !matches!(d.candidate, Candidate::Nothing);
    if d.scope != Scope::Lasting && acts {
        errs.push(format!("scope {:?} can only propose nothing", d.scope));
    }
    if let Candidate::Jig { jig } = &d.candidate {
        let n = d.cases.len();
        if !(3..=9).contains(&n) {
            errs.push(format!("a jig needs 3-9 validation cases, got {n}"));
        }
        let has = |e| d.cases.iter().any(|c| c.expect == e);
        if !has(Expect::Hit) || !has(Expect::Pass) {
            errs.push("cases must include at least one hit and one pass".into());
        }
        if !d
            .cases
            .iter()
            .any(|c| c.from_episode && c.expect == Expect::Hit)
        {
            errs.push(
                "no hit case from an episode: the jig must catch the friction it came from".into(),
            );
        }
        if jig.lifecycle != crate::jig::Lifecycle::Shadow {
            errs.push("a proposed jig enters in shadow".into());
        }
    }
    if let Candidate::Rule { file, text } = &d.candidate
        && (file.trim().is_empty() || text.trim().is_empty())
    {
        errs.push("a rule needs the file it goes in and its text".into());
    }
    errs
}

/// The result of running one case.
#[derive(Debug, Clone, PartialEq)]
pub enum CaseResult {
    Hit,
    Pass,
    /// The jig itself failed on this case.
    Error(String),
}

/// One replayed hit: did its call run, and is it a confirmed hit (its input
/// is a validation case that expects a hit)?
pub struct ReplayHit {
    pub ran: bool,
    pub confirmed: bool,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateReport {
    pub passed: bool,
    pub failures: Vec<String>,
    pub cases_ok: usize,
    pub cases: usize,
    pub replay_hits: usize,
    /// Hits on calls that ran normally and that no case confirms.
    pub unconfirmed: usize,
    pub unconfirmed_samples: Vec<String>,
}

/// Unconfirmed share of hits a nudge may have (Tricorder: under 10% for
/// results shown in review). A block may have none. Draft assumptions.
pub const NUDGE_BUDGET: f64 = 0.10;

pub fn gate(action: Action, cases: &[(Case, CaseResult)], hits: &[ReplayHit]) -> GateReport {
    let mut failures = vec![];
    let mut ok = 0;
    for (c, got) in cases {
        let want = match c.expect {
            Expect::Hit => CaseResult::Hit,
            Expect::Pass => CaseResult::Pass,
        };
        if *got == want {
            ok += 1;
        } else {
            failures.push(format!(
                "case {} {:?}: want {:?}, got {got:?}",
                c.tool_name, c.note, c.expect
            ));
        }
    }
    let unconfirmed: Vec<&ReplayHit> = hits.iter().filter(|h| h.ran && !h.confirmed).collect();
    let n = unconfirmed.len();
    let over = match action {
        Action::Block => n > 0,
        Action::Nudge => !hits.is_empty() && (n as f64 / hits.len() as f64) >= NUDGE_BUDGET,
    };
    if over {
        failures.push(format!(
            "{n} of {} replayed hits are on calls that ran and no case confirms ({} budget)",
            hits.len(),
            match action {
                Action::Block => "block: 0".to_owned(),
                Action::Nudge => format!("nudge: under {:.0}%", NUDGE_BUDGET * 100.0),
            }
        ));
    }
    GateReport {
        passed: failures.is_empty(),
        failures,
        cases_ok: ok,
        cases: cases.len(),
        replay_hits: hits.len(),
        unconfirmed: n,
        unconfirmed_samples: unconfirmed
            .iter()
            .take(5)
            .map(|h| h.evidence.clone())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jig::{Lifecycle, Rule};
    use serde_json::json;

    fn jig() -> Jig {
        Jig {
            id: "j".into(),
            description: String::new(),
            rule: Rule::BashRegex {
                pattern: "x".into(),
            },
            action: Action::Nudge,
            lifecycle: Lifecycle::Shadow,
            message: "m".into(),
            when: Default::default(),
            repeat: Default::default(),
        }
    }

    fn case(expect: Expect, from_episode: bool) -> Case {
        Case {
            tool_name: "Bash".into(),
            tool_input: json!({"command": "x"}),
            expect,
            from_episode,
            note: String::new(),
        }
    }

    fn draft(candidate: Candidate, scope: Scope, cases: Vec<Case>) -> Draft {
        Draft {
            episodes: vec![json!({"kind": "rework", "ts_ms": 1})],
            intent: "i".into(),
            scope,
            reason: "r".into(),
            candidate,
            cases,
            changed_since: None,
            replaces: None,
            scripts: Default::default(),
        }
    }

    #[test]
    fn shape_rules_for_jigs_and_scopes() {
        let good = vec![
            case(Expect::Hit, true),
            case(Expect::Pass, false),
            case(Expect::Pass, false),
        ];
        assert!(
            check_shape(&draft(
                Candidate::Jig { jig: jig() },
                Scope::Lasting,
                good.clone()
            ))
            .is_empty()
        );
        // Too few cases, and no episode hit.
        let errs = check_shape(&draft(
            Candidate::Jig { jig: jig() },
            Scope::Lasting,
            vec![case(Expect::Hit, false)],
        ));
        assert_eq!(errs.len(), 3, "{errs:?}");
        // A task-only correction cannot become a jig.
        let errs = check_shape(&draft(Candidate::Jig { jig: jig() }, Scope::ThisTask, good));
        assert!(
            errs.iter().any(|e| e.contains("only propose nothing")),
            "{errs:?}"
        );
        assert!(check_shape(&draft(Candidate::Nothing, Scope::Ambiguous, vec![])).is_empty());
        let mut enforce = jig();
        enforce.lifecycle = Lifecycle::Enforce;
        let errs = check_shape(&draft(
            Candidate::Jig { jig: enforce },
            Scope::Lasting,
            vec![
                case(Expect::Hit, true),
                case(Expect::Pass, false),
                case(Expect::Pass, false),
            ],
        ));
        assert!(errs.iter().any(|e| e.contains("shadow")));
    }

    fn hit(ran: bool, confirmed: bool) -> ReplayHit {
        ReplayHit {
            ran,
            confirmed,
            evidence: "e".into(),
        }
    }

    #[test]
    fn gate_counts_failed_cases_and_unconfirmed_hits() {
        let cases = vec![
            (case(Expect::Hit, true), CaseResult::Hit),
            (case(Expect::Pass, false), CaseResult::Pass),
        ];
        // 1 unconfirmed of 20 = 5%: a nudge passes, a block does not.
        let mut hits: Vec<ReplayHit> = (0..19).map(|_| hit(true, true)).collect();
        hits.push(hit(true, false));
        assert!(gate(Action::Nudge, &cases, &hits).passed);
        assert!(!gate(Action::Block, &cases, &hits).passed);
        // A hit on a call that did not run (denied, cut in) is not counted.
        assert!(gate(Action::Block, &cases, &[hit(false, false)]).passed);
        let bad = vec![(case(Expect::Pass, false), CaseResult::Hit)];
        let r = gate(Action::Nudge, &bad, &[]);
        assert_eq!((r.passed, r.cases_ok), (false, 0));
    }
}
