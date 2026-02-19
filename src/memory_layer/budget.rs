//! E7.4: Budget-aware context assembler (ADR-0004).
//! Greedy knapsack: maximize sum(utility_i) s.t. sum(tokens_i) <= budget.

use serde::{Deserialize, Serialize};

use super::ranker::Candidate;

// ── Types ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DroppedCandidate {
    pub rel_path: String,
    pub reason: String,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetReport {
    pub token_budget: u32,
    pub estimated_used: u32,
    pub candidates_total: usize,
    pub candidates_selected: usize,
    /// Fraction of budget consumed: estimated_used / token_budget.
    pub efficiency_score: f32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AssemblyResult {
    pub selected: Vec<Candidate>,
    pub dropped: Vec<DroppedCandidate>,
    pub budget_report: BudgetReport,
    /// Human-readable trace of why each selected candidate was chosen.
    pub decision_trace: Vec<String>,
}

// ── Token estimator ────────────────────────────────────────────────────────────

const BASE_TOKENS_PER_FILE: u32 = 40; // path + metadata overhead
const TOKENS_PER_CHAR: f32 = 0.28; // empirical: ~3.5 chars per token

/// Estimate token cost for a file by extension (rough median line-count-based heuristic).
pub fn estimate_tokens_for_path(rel_path: &str) -> u32 {
    let base = BASE_TOKENS_PER_FILE;
    let path_tokens = (rel_path.len() as f32 * TOKENS_PER_CHAR) as u32;
    let content_tokens: u32 = match rel_path.rsplit('.').next().unwrap_or("") {
        "rs" | "ts" | "tsx" | "java" | "go" | "cpp" | "c" => 350,
        "py" | "js" | "jsx" | "swift" | "kt" => 280,
        "md" | "toml" | "yaml" | "yml" => 150,
        "json" | "lock" => 120,
        _ => 200,
    };
    base + path_tokens + content_tokens
}

/// Utility of a candidate: score / normalized_token_cost.
/// Higher score and lower token cost = higher utility.
fn utility(c: &Candidate) -> f32 {
    let cost_normalized = (c.estimated_tokens.max(1) as f32 / 100.0).max(0.1);
    c.score / cost_normalized
}

// ── Assembly ───────────────────────────────────────────────────────────────────

/// Greedy budget-aware assembly (ADR-0004 §7.5).
/// Candidates must already be ranked (score set by ranker). Selects greedily by
/// utility-per-token, respecting the hard `token_budget` cap.
pub fn assemble(candidates: Vec<Candidate>, token_budget: u32) -> AssemblyResult {
    let total = candidates.len();
    let mut selected: Vec<Candidate> = Vec::new();
    let mut dropped: Vec<DroppedCandidate> = Vec::new();
    let mut tokens_used: u32 = 0;
    let mut trace: Vec<String> = Vec::new();

    // Sort by utility descending (greedy)
    let mut ordered = candidates;
    ordered.sort_by(|a, b| {
        utility(b)
            .partial_cmp(&utility(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for candidate in ordered {
        let cost = candidate.estimated_tokens;
        if tokens_used + cost <= token_budget {
            trace.push(format!(
                "{} selected (score={:.2}, est_tokens={}, utility={:.4}, sources=[{}])",
                candidate.rel_path,
                candidate.score,
                cost,
                utility(&candidate),
                candidate.sources.join(","),
            ));
            tokens_used += cost;
            selected.push(candidate);
        } else {
            dropped.push(DroppedCandidate {
                rel_path: candidate.rel_path.clone(),
                reason: format!(
                    "budget_exceeded (needs {}, available {})",
                    cost,
                    token_budget.saturating_sub(tokens_used),
                ),
                score: candidate.score,
            });
        }
    }

    let efficiency = if token_budget > 0 {
        tokens_used as f32 / token_budget as f32
    } else {
        0.0
    };

    AssemblyResult {
        budget_report: BudgetReport {
            token_budget,
            estimated_used: tokens_used,
            candidates_total: total,
            candidates_selected: selected.len(),
            efficiency_score: efficiency,
        },
        decision_trace: trace,
        selected,
        dropped,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_layer::ranker::Candidate;

    fn make_scored(path: &str, score: f32, tokens: u32) -> Candidate {
        let mut c = Candidate::new(path);
        c.score = score;
        c.estimated_tokens = tokens;
        c
    }

    #[test]
    fn test_assemble_respects_budget() {
        let candidates = vec![
            make_scored("a.rs", 0.9, 500),
            make_scored("b.rs", 0.8, 500),
            make_scored("c.rs", 0.7, 500),
        ];
        let result = assemble(candidates, 900);
        assert!(result.budget_report.estimated_used <= 900);
        // Only 1 can fit (each is 500 tokens, budget 900)
        assert_eq!(result.budget_report.candidates_selected, 1);
        assert_eq!(result.dropped.len(), 2);
    }

    #[test]
    fn test_assemble_maximizes_utility_not_just_score() {
        // cheap high-utility should beat expensive higher-score
        let candidates = vec![
            make_scored("cheap.rs", 0.8, 100),     // utility = 0.8 / 1.0 = 0.80
            make_scored("expensive.rs", 0.9, 900), // utility = 0.9 / 9.0 = 0.10
        ];
        let result = assemble(candidates, 500);
        assert_eq!(result.selected[0].rel_path, "cheap.rs");
    }

    #[test]
    fn test_budget_report_efficiency() {
        let candidates = vec![make_scored("a.rs", 0.9, 500)];
        let result = assemble(candidates, 1000);
        assert!(result.budget_report.efficiency_score <= 1.0);
        assert_eq!(result.budget_report.estimated_used, 500);
        assert!((result.budget_report.efficiency_score - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_dropped_candidates_have_reason() {
        let candidates = vec![
            make_scored("big.rs", 0.9, 9000),
            make_scored("small.rs", 0.5, 50),
        ];
        let result = assemble(candidates, 100);
        assert!(!result.dropped.is_empty());
        assert!(result
            .dropped
            .iter()
            .any(|d| d.reason.contains("budget_exceeded")));
    }

    #[test]
    fn test_empty_candidates() {
        let result = assemble(vec![], 3000);
        assert_eq!(result.selected.len(), 0);
        assert_eq!(result.budget_report.estimated_used, 0);
        assert_eq!(result.budget_report.efficiency_score, 0.0);
    }

    #[test]
    fn test_decision_trace_populated() {
        let candidates = vec![make_scored("a.rs", 0.8, 100)];
        let result = assemble(candidates, 500);
        assert!(!result.decision_trace.is_empty());
        assert!(result.decision_trace[0].contains("a.rs"));
    }

    #[test]
    fn test_estimate_tokens_by_extension() {
        assert!(estimate_tokens_for_path("src/main.rs") > estimate_tokens_for_path("config.json"));
        assert!(estimate_tokens_for_path("README.md") < estimate_tokens_for_path("src/lib.rs"));
    }
}
