//! Foundation for the online half — the part that needs a live model.
//!
//! Nothing here calls an API, and deliberately so: the online half answers a
//! different question ("given this context, does the model get it right?") and
//! must not reuse the offline half's results as if they were evidence for it.
//! What this module provides is the contract — a `Responder` to implement, a
//! task manifest any runner can consume, and a grader.
//!
//! The grader is a substring check and never a judge. That is possible only
//! because every generated fact carries a unique, checkable answer; it is the
//! single design choice that keeps the online half affordable.

use serde::{Deserialize, Serialize};

use crate::arms::Arm;
use crate::generate::Corpus;

/// Anything that can answer a prompt: an SDK client, a local runtime, a
/// recorded fixture.
pub trait Responder {
    fn respond(&self, system: &str, user: &str) -> anyhow::Result<String>;
}

pub const SYSTEM: &str = "Answer only from the context below, and name the component \
you are answering about. If the context does not contain the answer, reply exactly: \
NOT IN MEMORY.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    /// Which retrieval arm produced the context.
    pub arm: String,
    pub context: String,
    pub question: String,
    /// Substring a correct answer must contain; empty = the honest answer is
    /// "NOT IN MEMORY".
    pub expect: String,
    /// The invented subject the answer must also name. Slot values repeat
    /// across facts — two components can both "re-emit duplicates" — so the
    /// answer alone would credit a response drawn from the wrong node. The
    /// subject is unique by construction, which makes grading exact.
    pub expect_subject: String,
    pub context_tokens: usize,
}

impl Task {
    pub fn prompt(&self) -> String {
        format!("Context:\n{}\n\nQuestion: {}", self.context, self.question)
    }

    pub fn answerable(&self) -> bool {
        !self.expect.is_empty()
    }

    /// No judge, no rubric: either the checkable tokens are there or they are
    /// not. Both are required — the answer proves recall, the subject proves
    /// it came from the right node.
    pub fn grade(&self, answer: &str) -> bool {
        let a = answer.to_lowercase();
        if self.answerable() {
            a.contains(&self.expect.to_lowercase())
                && (self.expect_subject.is_empty()
                    || a.contains(&self.expect_subject.to_lowercase()))
        } else {
            a.contains("not in memory")
        }
    }
}

/// Build the manifest for one arm: every question, plus the unanswerable
/// controls, with exactly the context that arm would have delivered.
pub fn tasks(corpus: &Corpus, arm: &dyn Arm, limit: usize) -> Vec<Task> {
    let mut out = Vec::new();
    let answerable = corpus
        .questions()
        .filter_map(|q| q.gold.as_ref().map(|g| (q, g.clone())));

    for (i, (q, gold)) in answerable.enumerate() {
        let f = corpus.fact(&gold);
        let expect = f.map(|f| f.answer.clone()).unwrap_or_default();
        let subject = f.map(|f| f.subject.clone()).unwrap_or_default();
        out.push(task(
            arm,
            limit,
            format!("a{i:04}"),
            &q.text,
            expect,
            subject,
        ));
    }
    for (i, q) in corpus.unanswerable.iter().enumerate() {
        out.push(task(
            arm,
            limit,
            format!("u{i:04}"),
            &q.text,
            String::new(),
            String::new(),
        ));
    }
    out
}

fn task(
    arm: &dyn Arm,
    limit: usize,
    id: String,
    question: &str,
    expect: String,
    expect_subject: String,
) -> Task {
    let r = arm.retrieve(question, limit);
    // What the arm actually delivered, verbatim — NOT the corpus text behind
    // the keys it returned. Rebuilding from full note bodies here is how an
    // earlier version handed every ranked arm the same context and erased the
    // thing under test: an arm that returns snippets was being measured as if
    // it had returned whole records.
    let context = r.rendered.join("\n\n");
    Task {
        id,
        arm: arm.name().to_string(),
        question: question.to_string(),
        expect,
        expect_subject,
        context_tokens: r.tokens,
        context,
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OnlineReport {
    pub arm: String,
    pub answerable: usize,
    pub answerable_correct: usize,
    pub unanswerable: usize,
    /// Said "NOT IN MEMORY" when it had nothing — the honesty metric.
    pub unanswerable_correct: usize,
    pub context_tokens_mean: f64,
}

pub fn run(responder: &dyn Responder, tasks: &[Task]) -> anyhow::Result<OnlineReport> {
    let mut r = OnlineReport {
        arm: tasks.first().map(|t| t.arm.clone()).unwrap_or_default(),
        ..Default::default()
    };
    for t in tasks {
        let answer = responder.respond(SYSTEM, &t.prompt())?;
        let ok = t.grade(&answer);
        if t.answerable() {
            r.answerable += 1;
            r.answerable_correct += usize::from(ok);
        } else {
            r.unanswerable += 1;
            r.unanswerable_correct += usize::from(ok);
        }
    }
    r.context_tokens_mean =
        tasks.iter().map(|t| t.context_tokens as f64).sum::<f64>() / tasks.len().max(1) as f64;
    Ok(r)
}

/// Answers perfectly whenever the context actually contains the answer — the
/// ceiling any real model is measured against, and what the harness tests use.
pub struct OracleResponder;

impl Responder for OracleResponder {
    fn respond(&self, _system: &str, user: &str) -> anyhow::Result<String> {
        Ok(user.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arms::{GrepArm, WholeFileArm};
    use crate::generate::corpus;

    #[test]
    fn grading_needs_no_judge() {
        let t = Task {
            id: "a0".into(),
            arm: "grep".into(),
            context: String::new(),
            question: "q".into(),
            expect: "7 attempts".into(),
            expect_subject: "Kelnor lease broker".into(),
            context_tokens: 0,
        };
        assert!(t.grade("The Kelnor lease broker settled on 7 Attempts."));
        assert!(!t.grade("about eight attempts for the Kelnor lease broker"));
        assert!(!t.grade("NOT IN MEMORY"));
        // Right value, wrong node: slot values repeat across facts, so this
        // is exactly the response the subject check exists to reject.
        assert!(!t.grade("The Vantis shard router uses 7 attempts."));
    }

    #[test]
    fn an_unanswerable_task_wants_an_admission() {
        let t = Task {
            id: "u0".into(),
            arm: "grep".into(),
            context: String::new(),
            question: "q".into(),
            expect: String::new(),
            expect_subject: String::new(),
            context_tokens: 0,
        };
        assert!(!t.answerable());
        assert!(t.grade("NOT IN MEMORY"));
        assert!(!t.grade("It is 12 seconds."));
    }

    #[test]
    fn manifest_covers_answerable_and_control_tasks() {
        let c = corpus(20, 40, 1);
        let arm = GrepArm::new(&c);
        let tasks = tasks(&c, &arm, 5);
        assert_eq!(
            tasks.iter().filter(|t| t.answerable()).count(),
            c.questions().count()
        );
        assert_eq!(
            tasks.iter().filter(|t| !t.answerable()).count(),
            c.unanswerable.len()
        );
        assert!(tasks.iter().all(|t| t.arm == "grep"));
    }

    #[test]
    fn a_task_carries_the_context_its_arm_actually_delivered() {
        // The invariant that keeps the online half honest: what goes in the
        // prompt must be the same text the offline half billed for. When these
        // drift, every cost-per-answer number silently describes a different
        // system than the one that produced the recall numbers.
        //
        // Checked as a token count rather than byte equality because the
        // manifest adds `## ` headers and blank lines between records — prompt
        // scaffolding the arm is not charged for.
        let c = corpus(20, 40, 7);
        let arm = GrepArm::new(&c);
        for t in tasks(&c, &arm, 5) {
            if t.context_tokens == 0 {
                continue;
            }
            let actual = crate::arms::tokens(&t.context) as f64;
            let billed = t.context_tokens as f64;
            assert!(
                (actual - billed).abs() / billed < 0.15,
                "task {} billed {billed} tokens but its prompt carries {actual}",
                t.id
            );
        }
    }

    #[test]
    fn the_whole_file_arm_puts_everything_in_every_prompt() {
        let c = corpus(20, 40, 2);
        let arm = WholeFileArm::new(&c);
        let tasks = tasks(&c, &arm, 5);
        // The dump now renders one entry per record (the focus metric needs
        // the answer separable from its surroundings); joined back together it
        // is the flat file, minus only the trailing separator.
        let flat = c.flat_file();
        assert!(tasks.iter().all(|t| t.context == flat.trim_end()));
    }

    #[test]
    fn the_oracle_scores_what_the_context_contained() {
        let c = corpus(20, 40, 3);
        let arm = WholeFileArm::new(&c);
        let tasks = tasks(&c, &arm, 5);
        let r = run(&OracleResponder, &tasks).unwrap();
        // Everything is in context, so every answerable task is winnable...
        assert_eq!(r.answerable_correct, r.answerable);
        // ...and the control tasks are exactly what a full dump gets wrong.
        assert_eq!(r.unanswerable_correct, 0);
        assert!(r.context_tokens_mean > 0.0);
    }
}
