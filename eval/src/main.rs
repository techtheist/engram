use std::process::ExitCode;

use engram_eval::arms::{Arm, EngramArm, GrepArm, RagArm, WholeFileArm};
use engram_eval::generate::{KINDS, Kind, Phrasing, corpus};
use engram_eval::metrics::PhrasingMix;
use engram_eval::online;
use engram_eval::run::{Config, Report, run};

const USAGE: &str = "\
engram-eval — offline retrieval suite

USAGE:
    engram-eval [OPTIONS]

OPTIONS:
    --sizes 50,200,500    tested facts per run       [default: 50,200]
    --distractors N       noise facts per tested one [default: 2]
    --type-mix K=W,...    question weight per node type
                          [default: decision=35,caution=20,insight=20,
                                    problem=15,principle=10]
    --terse               short, edgeless notes — the shape this harness
                          used before it was profiled against a real graph
    --embed-model NAME    bge-small-en-v1.5 (default) | bge-base-en-v1.5 |
                          all-MiniLM-L6-v2 — must already be provisioned
                          under ~/.cache/engram
    --seed N              corpus seed                [default: 1]
    --limit N             results an arm may return  [default: 10]
    --nli-budget N        pairs to judge per size    [default: 300]
    --curated-budget N[,N]  token budget(s) for the hand-maintained-file
                          baseline — one arm per budget [default: 3000]
    --ladder              the gradation series: TOTAL graph sizes
                          10,100,200,500,1000,1500 with EVERY fact questioned
                          (no untested distractors, uniform type mix) and the
                          curated file scored at 3,000 AND 30,000 tokens —
                          answers \"where does a maintained file lose?\".
                          Flags after --ladder still override its presets
    --series              the whole battery in one command: --ladder plus the
                          contradiction bench, combined JSON written to
                          eval-series.json (or --json PATH). Expect a long
                          real-embeddings run
    --phrasing-mix L,P,O  how often each phrasing is assumed to occur
                          [default: 45,45,10] — decides the headline number
    --tricks              research bench: candidate delivery strategies
                          (fixed/relative floors, knee cut, knee+buffer,
                          split-conformal abstention calibrated on synthetic
                          never-written probes) scored from one recorded pass
                          per size. Research only — nothing in it ships
    --qpp                 research bench: per-query QPP signals (score-curve
                          shape features + pool-bottom z) scored as
                          answerable-vs-control AUC next to the shipped
                          phantom weak line. The probe-register-gap attack:
                          a per-query null carries the query's own register
    --rerank-full         rerank on title + FULL note body instead of the
                          keyword-window snippet (research candidate; works
                          under any mode's engram arm)
    --posttune            measure the SHIPPED post-tune stack end to end at
                          each size: knee trim on, weak line calibrated by
                          auto-tune's phantom-probe dial, FP scored under the
                          recommendation regime (a warned answer to a
                          never-written question counts as honest). The arms
                          table's \"engram (post-tune)\" row comes from this
    --floor               sweep a delivery floor over the engram arm: per
                          candidate floor, what abstention on unanswerable
                          questions costs in recall, and what trimming the
                          weak tail buys in focus/noise. The calibrated-
                          delivery default comes from this table
    --sweep               grid-search the fusion balance against the semantic
                          floor, printed against what pure vectors score
    --bench               run candidate retrieval strategies that do NOT ship
                          (rank fusion, deeper reranking, graph spreading)
                          over one corpus, against rag and the shipped stack
    --contradictions      score the contradiction layer end to end: catch
                          rate, false alarms, and whether the misses were
                          retrieval's fault or the model's
    --real-graph PATH     score the suspect queue against a REAL graph's
                          judged history: every pair a human ruled on, plus
                          every conflicts-with edge. Point it at a COPY — a
                          running daemon owns the original
    --no-rerank           drop the cross-encoder — a diagnostic, not an option
    --flat-priors         zero every type's rank_prior — also a diagnostic
    --sample              print sample generated facts and exit
    --json PATH           write the full report as JSON
    --emit-tasks PATH     write the online-half task manifest as JSON
    -h, --help

To isolate density from corpus size, hold --sizes fixed and vary
--distractors: the tested facts and their questions stay byte-identical
while the graph around them grows.

Build with --features fastembed for real embeddings; without it the numbers
exercise the harness and the lexical path only.
";

fn main() -> ExitCode {
    match cli() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("engram-eval: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cli() -> anyhow::Result<()> {
    let mut cfg = Config::default();
    let mut json_out: Option<String> = None;
    let mut tasks_out: Option<String> = None;
    let mut sample = false;
    let mut floor_mode = false;
    let mut tricks_mode = false;
    let mut qpp_mode = false;
    let mut posttune_mode = false;
    let mut sweep_mode = false;
    let mut bench_mode = false;
    let mut contradiction_mode = false;
    let mut ladder_mode = false;
    let mut series_mode = false;
    let mut real_graph: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || -> anyhow::Result<String> {
            args.next()
                .ok_or_else(|| anyhow::anyhow!("{arg} needs a value"))
        };
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--sizes" => {
                cfg.sizes = value()?
                    .split(',')
                    .map(|s| s.trim().parse::<usize>())
                    .collect::<Result<_, _>>()?;
            }
            "--distractors" => cfg.distractor_ratio = value()?.parse()?,
            "--type-mix" => cfg.type_mix = parse_type_mix(&value()?)?,
            "--terse" => cfg.profile = engram_eval::profile::Profile::terse(),
            "--phrasing-mix" => cfg.phrasing = parse_phrasing(&value()?)?,
            "--ladder" => {
                ladder_mode = true;
                apply_ladder(&mut cfg);
            }
            "--series" => {
                ladder_mode = true;
                series_mode = true;
                apply_ladder(&mut cfg);
            }
            "--floor" => floor_mode = true,
            "--tricks" => tricks_mode = true,
            "--qpp" => qpp_mode = true,
            "--posttune" => posttune_mode = true,
            "--rerank-full" => cfg.rerank_full = true,
            "--sweep" => sweep_mode = true,
            "--bench" => bench_mode = true,
            "--contradictions" => contradiction_mode = true,
            "--real-graph" => real_graph = Some(value()?),
            "--no-rerank" => cfg.no_rerank = true,
            "--flat-priors" => cfg.flat_priors = true,
            "--sample" => sample = true,
            "--embed-model" => cfg.embed_model = Some(value()?),
            "--seed" => cfg.seed = value()?.parse()?,
            "--limit" => cfg.limit = value()?.parse()?,
            "--nli-budget" => cfg.nli_budget = value()?.parse()?,
            "--curated-budget" => {
                cfg.curated_budgets = value()?
                    .split(',')
                    .map(|s| s.trim().parse::<usize>())
                    .collect::<Result<_, _>>()?;
            }
            "--json" => json_out = Some(value()?),
            "--emit-tasks" => tasks_out = Some(value()?),
            other => anyhow::bail!("unknown option {other} (try --help)"),
        }
    }

    if cfg.sizes.is_empty() {
        anyhow::bail!("--sizes needs at least one corpus size");
    }

    if sample {
        print_sample(&cfg);
        return Ok(());
    }
    if floor_mode {
        let report = engram_eval::run::floor_sweep(&cfg)?;
        print_floor(&report);
        if let Some(path) = json_out {
            std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
            println!("\nwrote {path}");
        }
        return Ok(());
    }
    if tricks_mode {
        let report = engram_eval::run::tricks(&cfg)?;
        print_tricks(&report);
        if let Some(path) = json_out {
            std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
            println!("\nwrote {path}");
        }
        return Ok(());
    }

    if qpp_mode {
        let report = engram_eval::run::qpp(&cfg)?;
        print_qpp(&report);
        if let Some(path) = json_out {
            std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
            println!("\nwrote {path}");
        }
        return Ok(());
    }
    if posttune_mode {
        let report = engram_eval::run::posttune(&cfg)?;
        print_posttune(&report);
        if let Some(path) = json_out {
            std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
            println!("\nwrote {path}");
        }
        return Ok(());
    }
    if sweep_mode {
        print_sweep(&engram_eval::run::sweep(&cfg)?, &cfg);
        return Ok(());
    }
    if let Some(path) = real_graph {
        let report = engram_eval::run::real_graph(&path)?;
        print_real_graph(&report);
        if let Some(path) = json_out {
            std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
            println!("\nwrote {path}");
        }
        return Ok(());
    }
    if contradiction_mode {
        let report = engram_eval::run::contradictions(&cfg)?;
        print_contradictions(&report);
        if let Some(path) = json_out {
            std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
            println!("\nwrote {path}");
        }
        return Ok(());
    }
    if bench_mode {
        let report = engram_eval::run::bench(&cfg)?;
        print_bench(&report);
        if let Some(path) = json_out {
            std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
            println!("\nwrote {path}");
        }
        return Ok(());
    }

    let report = run(&cfg)?;
    print_report(&report);
    if ladder_mode {
        print_ladder(&report);
    }

    if series_mode {
        // The contradiction bench picks its own size: the ladder's first rung
        // (10 facts) is far too small an instrument for a catch rate.
        let mut ccfg = cfg.clone();
        ccfg.sizes = vec![500];
        let contradictions = engram_eval::run::contradictions(&ccfg)?;
        println!();
        print_contradictions(&contradictions);
        let path = json_out.unwrap_or_else(|| "eval-series.json".to_string());
        let combined = serde_json::json!({
            "ladder": report,
            "contradictions": contradictions,
        });
        std::fs::write(&path, serde_json::to_string_pretty(&combined)?)?;
        println!("\nwrote {path}");
    } else if let Some(path) = json_out {
        std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
        println!("\nwrote {path}");
    }
    if let Some(path) = tasks_out {
        let n = emit_tasks(&cfg, &path)?;
        println!("wrote {n} online tasks to {path}");
    }
    Ok(())
}

/// The gradation presets: sizes are TOTAL graph sizes and every fact is
/// questioned — no untested distractors, no type-mix thinning. Assumed
/// workload mixes belong in report-side weighting, not in which questions get
/// asked; the phrasing weighting already works that way.
fn apply_ladder(cfg: &mut Config) {
    cfg.sizes = vec![10, 100, 200, 500, 1000, 1500];
    cfg.distractor_ratio = 0;
    cfg.type_mix = KINDS.iter().map(|k| (*k, 1)).collect();
    cfg.curated_budgets = vec![3000, 30000];
}

/// The ladder's own summary: recall@5 by graph size, one column per arm worth
/// following, and the crossover sentence the write-up needs — the first size
/// at which each curated budget falls behind retrieval.
fn print_ladder(r: &Report) {
    let followed: Vec<String> = r
        .sizes
        .first()
        .map(|s| {
            s.curated
                .iter()
                .map(|c| c.arm.clone())
                .chain(["grep", "rag", "engram-hybrid"].map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if followed.is_empty() {
        return;
    }

    println!("\nengram-eval — the ladder: recall@5 by graph size");
    print!("  {:>7} {:>6}", "graph", "asked");
    for name in &followed {
        print!(" {name:>15}");
    }
    println!();
    let recall = |s: &engram_eval::run::SizeReport, name: &str| -> Option<f64> {
        s.arms
            .iter()
            .find(|a| a.arm == name)
            .map(|a| a.overall.recall_at_5)
    };
    for s in &r.sizes {
        print!("  {:>7} {:>6}", s.graph, s.questions);
        for name in &followed {
            match recall(s, name) {
                Some(v) => print!(" {v:>15.2}"),
                None => print!(" {:>15}", "-"),
            }
        }
        println!();
    }

    for c in &r.sizes.first().expect("non-empty").curated {
        let lost = r.sizes.iter().find(|s| {
            let cur = recall(s, &c.arm);
            let eng = recall(s, "engram-hybrid");
            matches!((cur, eng), (Some(cur), Some(eng)) if cur < eng)
        });
        match lost {
            Some(s) => {
                let held = s
                    .curated
                    .iter()
                    .find(|x| x.arm == c.arm)
                    .map(|x| x.held)
                    .unwrap_or_default();
                println!(
                    "  {} falls behind retrieval at {} notes ({:.2} vs {:.2}, holding {} of {})",
                    c.arm,
                    s.graph,
                    recall(s, &c.arm).unwrap_or_default(),
                    recall(s, "engram-hybrid").unwrap_or_default(),
                    held,
                    s.graph
                );
            }
            None => println!(
                "  {} never falls behind retrieval on this ladder — extend the sizes",
                c.arm
            ),
        }
    }
}

/// `45,45,10` — lexical, paraphrase, oblique. Normalised, so any scale works.
fn parse_phrasing(spec: &str) -> anyhow::Result<PhrasingMix> {
    let parts: Vec<f64> = spec
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()?;
    let [lexical, paraphrase, oblique] = parts[..] else {
        anyhow::bail!("--phrasing-mix wants three numbers: lexical,paraphrase,oblique");
    };
    if lexical + paraphrase + oblique <= 0.0 {
        anyhow::bail!("--phrasing-mix cannot be all zeroes");
    }
    Ok(PhrasingMix {
        lexical,
        paraphrase,
        oblique,
    })
}

fn print_posttune(r: &engram_eval::run::PostTuneReport) {
    println!("engram-eval — the shipped post-tune stack, measured end to end");
    println!(
        "runtime: embedder={}  reranker={}  seed={}  limit={}",
        r.embedder, r.reranker, r.seed, r.limit
    );
    if r.embeddings_are_fake {
        println!("!! FAKE EMBEDDINGS — these numbers describe plumbing, not meaning");
    }
    for s in &r.sizes {
        println!(
            "\ngraph {} facts ({} edges) / {} questions / {} controls",
            s.graph, s.edges, s.questions, s.controls
        );
        println!(
            "  auto-tune: {} (weak line {:.3})",
            s.auto_tune_note
                .as_deref()
                .unwrap_or("left the defaults untouched"),
            s.weak_line
        );
        let at = |p: Phrasing| {
            s.by_phrasing
                .iter()
                .find(|ps| ps.phrasing == p)
                .map(|ps| ps.score.recall_at_5)
                .unwrap_or_default()
        };
        println!(
            "  recall (with graph credit): R@1 {:.2}  R@5 {:.2} (hybrid alone {:.2})  lex {:.2}  para {:.2}  obliq {:.2}  weighted {:.3}",
            s.assisted.recall_at_1,
            s.assisted.recall_at_5,
            s.overall.recall_at_5,
            at(Phrasing::Lexical),
            at(Phrasing::Paraphrase),
            at(Phrasing::Oblique),
            s.weighted_recall,
        );
        println!(
            "  attention: focus {:.2}  noise {:.2}  tok/query {:.0}  standing {}",
            s.overall.focus, s.overall.noise, s.overall.tokens_mean, s.standing_tokens
        );
        println!(
            "  recommendation regime: honest FP (controls unwarned) {:.2}  controls empty/none {:.2}  answerable warned {:.2}",
            s.controls_unwarned, s.controls_empty, s.answerable_warned
        );
    }
    println!(
        "\nReading it: candidates are never cut — below the auto-tuned weak line the reply\n\
         leads with \"likely not in memory\", so a warned answer to a never-written question\n\
         counts as honest and only an unwarned one is a false positive."
    );
}

fn print_tricks(r: &engram_eval::run::TricksReport) {
    println!("engram-eval — delivery-strategy research bench (nothing here ships)");
    println!(
        "runtime: embedder={}  reranker={}  seed={}  limit={}",
        r.embedder, r.reranker, r.seed, r.limit
    );
    if r.embeddings_are_fake {
        println!("!! FAKE EMBEDDINGS — every number below is noise");
    }
    for s in &r.sizes {
        println!(
            "\ngraph {} facts / {} questions / controls {} calibrate + {} evaluate  (conformal t: q90 {:.3}, q95 {:.3})",
            s.graph,
            s.questions,
            s.controls_calibration,
            s.controls_eval,
            s.conformal_q90,
            s.conformal_q95
        );
        println!(
            "  q90 as the per-graph weak line: {:.0}% of answerable label strong, {:.0}% of held-out controls flagged",
            100.0 * s.label_answerable_strong,
            100.0 * s.label_controls_flagged
        );
        if !s.label_grid.is_empty() {
            println!(
                "  recommendation regime (never cuts): q  threshold  ctrl-unwarned(FP)  ans-warned"
            );
            for p in &s.label_grid {
                println!(
                    "  {:>36.2} {:>10.3} {:>18.2} {:>11.2}",
                    p.quantile, p.threshold, p.controls_unwarned, p.answerable_warned
                );
            }
        }
        println!(
            "  {:<22} {:>6} {:>6} {:>6} {:>6} {:>6} {:>9} {:>9} {:>6}",
            "strategy", "R@5", "obliq", "focus", "noise", "kept", "tok/query", "decl-ans", "FP"
        );
        for row in &s.rows {
            println!(
                "  {:<22} {:>6.2} {:>6.2} {:>6.2} {:>6.2} {:>6.1} {:>9.0} {:>9.2} {:>6.2}",
                row.strategy,
                row.recall_at_5,
                row.oblique_recall_at_5,
                row.focus,
                row.noise,
                row.mean_returned,
                row.tokens_mean,
                row.declined_answerable,
                row.false_positive_rate,
            );
        }
    }
    println!(
        "\nReading it: FP is scored on the held-out control half only — the conformal\n\
         thresholds never see the probes they are judged on. decl-ans is what each\n\
         strategy's restraint costs on real questions; a strategy only earns its FP\n\
         number if R@5 and obliq survive next to it."
    );
}

fn print_qpp(r: &engram_eval::run::QppReport) {
    println!("engram-eval — per-query QPP bench (research only, nothing here ships)");
    println!(
        "runtime: embedder={}  reranker={}  seed={}  limit={}",
        r.embedder, r.reranker, r.seed, r.limit
    );
    if r.embeddings_are_fake {
        println!("!! FAKE EMBEDDINGS — every number below is noise");
    }
    for s in &r.sizes {
        println!(
            "\ngraph {} facts / {} answerable / {} controls",
            s.graph, s.answerable, s.controls
        );
        println!(
            "  shipped reference — phantom weak line {:.3}: FP (controls unwarned) {:.2}, answerable warned {:.2}",
            s.weak_line, s.weak_line_controls_unwarned, s.weak_line_answerable_warned
        );
        println!("  {:<15} {:>6}   (0.5 = blind; <0.5 = signal points the other way)", "feature", "AUC");
        for f in &s.features {
            println!("  {:<15} {:>6.3}", f.feature, f.auc);
        }
        println!(
            "  pool-bottom z gate: {:>5} {:>18} {:>11}",
            "z", "ctrl-unwarned(FP)", "ans-warned"
        );
        for p in &s.z_sweep {
            println!(
                "  {:>25.1} {:>18.2} {:>11.2}",
                p.z, p.controls_unwarned, p.answerable_warned
            );
        }
        println!(
            "  random-background z: {:>4} {:>18} {:>11}",
            "z", "ctrl-unwarned(FP)", "ans-warned"
        );
        for p in &s.z_rand_sweep {
            println!(
                "  {:>25.1} {:>18.2} {:>11.2}",
                p.z, p.controls_unwarned, p.answerable_warned
            );
        }
        println!(
            "  gumbel null-max gate: {:>3} {:>18} {:>11}",
            "p", "ctrl-unwarned(FP)", "ans-warned"
        );
        for p in &s.gumbel_sweep {
            println!(
                "  {:>25.2} {:>18.2} {:>11.2}",
                p.p, p.controls_unwarned, p.answerable_warned
            );
        }
        println!(
            "  local-crowd z (shoulder): {:>18} {:>11}",
            "ctrl-unwarned(FP)", "ans-warned"
        );
        for p in &s.z_shoulder_sweep {
            println!(
                "  {:>25.1} {:>18.2} {:>11.2}",
                p.z, p.controls_unwarned, p.answerable_warned
            );
        }
    }
    println!(
        "\nReading it: every feature is per-query arithmetic over the delivered score\n\
         curve — no per-graph calibration anywhere. A feature only matters if its AUC\n\
         beats the weak-line reference row at the SAME answerable-warned cost; the z\n\
         gate row to compare is the one whose ans-warned matches the reference's."
    );
}

fn print_floor(r: &engram_eval::run::FloorReport) {
    println!("engram-eval — delivery-floor sweep (engram arm)");
    println!(
        "runtime: embedder={}  reranker={}  seed={}  limit={}",
        r.embedder, r.reranker, r.seed, r.limit
    );
    if r.embeddings_are_fake {
        println!("!! FAKE EMBEDDINGS — every number below is noise");
    }
    for s in &r.sizes {
        println!(
            "\ngraph {} facts / {} questions / {} controls",
            s.graph, s.questions, s.controls
        );
        println!(
            "  {:>7} {:>6} {:>6} {:>9} {:>9} {:>6} {:>6} {:>6} {:>9}",
            "floor", "R@5", "obliq", "decl-ans", "decl-ctrl", "noise", "focus", "kept", "tok/query"
        );
        for p in &s.points {
            println!(
                "  {:>7.3} {:>6.2} {:>6.2} {:>9.2} {:>9.2} {:>6.2} {:>6.2} {:>6.1} {:>9.0}",
                p.floor,
                p.recall_at_5,
                p.oblique_recall_at_5,
                p.declined_answerable,
                p.controls_declined,
                p.noise,
                p.focus,
                p.mean_returned,
                p.tokens_mean,
            );
        }
    }
    println!(
        "\nReading it: `decl-ctrl` is the point — the share of never-written questions\n\
         the floor declines instead of answering with the nearest-looking thing.\n\
         `decl-ans` is what that restraint costs on real questions, and R@5 is what\n\
         survives after the trim. The shipped floor should sit where decl-ctrl is\n\
         high, decl-ans is negligible, and R@5 has not moved."
    );
}

fn print_sweep(report: &engram_eval::run::SweepReport, cfg: &Config) {
    println!("engram-eval — fusion sweep");
    println!(
        "weighted by an assumed mix of lexical={} paraphrase={} oblique={}\n",
        cfg.phrasing.lexical, cfg.phrasing.paraphrase, cfg.phrasing.oblique
    );
    println!(
        "  {:>7} {:>7} {:>9} {:>7} {:>7} {:>7} {:>9}",
        "kw", "floor", "weighted", "lex", "para", "obliq", "tok/query"
    );
    let r = &report.rag;
    println!(
        "  {:>7} {:>7} {:>9.3} {:>7.2} {:>7.2} {:>7.2} {:>9.0}   <- rag (pure vectors)",
        "-", "-", r.weighted_recall, r.lexical, r.paraphrase, r.oblique, r.tokens_mean
    );
    for p in &report.points {
        println!(
            "  {:>7.2} {:>7.2} {:>9.3} {:>7.2} {:>7.2} {:>7.2} {:>9.0}{}",
            p.keyword_weight,
            p.semantic_floor,
            p.weighted_recall,
            p.lexical,
            p.paraphrase,
            p.oblique,
            p.tokens_mean,
            if p.is_default {
                "   <- shipped today"
            } else {
                ""
            }
        );
    }
    println!(
        "\nHigher `kw` favours queries that name their subject; lower favours ones\n\
         that describe it. `floor` is the cosine under which a vector match counts\n\
         for nothing — and because a hit scoring zero relevance is skipped rather\n\
         than ranked last, the floor is a delete, not a demotion. A query that\n\
         names nothing scores zero on keywords too, so it is the one phrasing that\n\
         can trip both conditions at once and vanish before the reranker sees it.\n\
         There is no optimum here independent of how often each phrasing happens —\n\
         which is the assumption printed above, not a measurement."
    );
}

fn print_real_graph(r: &engram_eval::run::RealGraphReport) {
    println!("engram-eval — the suspect queue, on a real graph");
    println!("graph: {}  ({} nodes)", r.graph, r.nodes);
    println!("model: {}\n", r.model);
    println!(
        "  suspect rows              {:>6}   {} still pending",
        r.suspects, r.pending
    );
    println!("  scored pairs              {:>6}", r.judged);
    let n = |v: &str| r.pairs.iter().filter(|p| p.verdict == v).count();
    println!("    dismissed               {:>6}", n("dismiss"));
    println!("    confirmed — conflict    {:>6}", n("conflict"));
    println!("    confirmed — replaces    {:>6}", n("replaces"));
    if r.contradiction_hinted > 0 {
        println!(
            "\n  ! {} of these were queued because a PREVIOUS model called them a\n\
             \x20   contradiction, which is the trait being scored. The unbiased column\n\
             \x20   drops them; the other hinted rows came from the duplicate sweep,\n\
             \x20   which selects on the opposite label.",
            r.contradiction_hinted
        );
    }

    println!("\n  dismissed pairs the model calls a contradiction, three ways:");
    println!("    all      — every judged pair, however it got queued");
    println!("    unbiased — minus pairs a previous model preselected");
    println!("    queued   — minus already-linked pairs, which the sweep skips");
    println!(
        "\n  gate        all       unbiased        queued     conflicts   (floor {:.2})",
        r.similarity_floor
    );
    let pct = |n: usize, d: usize| {
        if d == 0 {
            0.0
        } else {
            n as f64 / d as f64 * 100.0
        }
    };
    for g in &r.gates {
        let live = (g.min_confidence - r.shipped_gate).abs() < 1e-9;
        println!(
            "  {:>5.2} {:>5}/{:<3}{:>4.0}% {:>5}/{:<3}{:>4.0}% {:>5}/{:<3}{:>4.0}% {:>7}/{:<3}{}",
            g.min_confidence,
            g.dismissed_flagged,
            g.dismissed,
            pct(g.dismissed_flagged, g.dismissed),
            g.unbiased_flagged,
            g.unbiased,
            pct(g.unbiased_flagged, g.unbiased),
            g.as_queued_flagged,
            g.as_queued,
            pct(g.as_queued_flagged, g.as_queued),
            g.conflicts_flagged,
            g.conflicts,
            if live { "   <- ships today" } else { "" }
        );
    }

    let noisy: Vec<_> = r
        .pairs
        .iter()
        .filter(|p| {
            p.verdict == "dismiss" && p.label == "contradiction" && p.score >= r.shipped_gate
        })
        .collect();
    if !noisy.is_empty() {
        println!(
            "\n  Pairs a human dismissed that the model still calls a contradiction\n  at the shipped gate — the queue noise this layer would produce:\n"
        );
        for p in &noisy {
            println!(
                "  {:.2}{}  {}",
                p.score,
                if p.linked { " [linked]" } else { "" },
                p.a_title
            );
            println!("        vs {}", p.b_title);
        }
        println!(
            "\n  [linked] = the two nodes carry an edge today, so the sweep would\n  skip the pair outright — noise the product would not actually produce."
        );
    }

    println!(
        "\nReading it: this corpus is small and nobody picked it — it is every pair\n\
         a human has ruled on in this graph, on real prose, already past the\n\
         similarity floor that guards the sweep. That makes it the right\n\
         instrument for the sweep's confidence gate and the WRONG one for a catch\n\
         rate: the positive class is only whatever this project happened to\n\
         contradict itself about, so read the conflicts column as named cases,\n\
         never as a percentage."
    );
}

fn print_contradictions(r: &engram_eval::run::ContradictionReport) {
    println!("engram-eval — contradiction layer, end to end");
    println!("model: {}\n", r.model);
    println!("  contradicting claims      {:>6}", r.contradictions);
    println!(
        "    caught                  {:>6}   {:.0}%",
        r.caught,
        r.catch_rate() * 100.0
    );
    println!("    missed — never retrieved{:>6}", r.missed_by_retrieval);
    println!("    missed — judged wrongly {:>6}", r.missed_by_judgment);
    println!(
        "  agreeing claims           {:>6}   {:.0}% confirmed",
        r.entailments,
        if r.entailments == 0 {
            0.0
        } else {
            r.supported as f64 / r.entailments as f64 * 100.0
        }
    );
    println!(
        "    missed — never retrieved{:>6}",
        r.agree_missed_by_retrieval
    );
    println!("    judged neutral          {:>6}", r.agree_judged_neutral);
    println!(
        "    judged CONTRADICTION    {:>6}",
        r.agree_judged_contradiction
    );
    println!(
        "  unrelated claims          {:>6}   {:.0}% false alarms",
        r.neutrals,
        r.false_alarm_rate() * 100.0
    );
    if !r.gates.is_empty() {
        println!("\n  confidence gate    catch   false alarms   agree-alarms");
        for g in &r.gates {
            let live = (g.min_confidence - r.shipped_gate).abs() < 1e-9;
            println!(
                "  {:>14.2} {:>7.0}% {:>13.0}% {:>13.0}%{}",
                g.min_confidence,
                g.catch_rate * 100.0,
                g.false_alarm_rate * 100.0,
                g.agree_alarm_rate * 100.0,
                if live { "   <- ships today" } else { "" }
            );
        }
    }
    println!(
        "\nReading it: `caught` is what the layer is for, but on its own it proves\n\
         nothing — a layer that shouts contradiction at everything scores 100%.\n\
         Read it against the false-alarm row, which is what such a layer costs.\n\
         The two miss rows split the failures by cause, which is the part that\n\
         decides anything: a claim whose target was never retrieved is not a\n\
         judgment the model got wrong, it is one the model never saw. {:.0}% of\n\
         misses were the model's, so that is the ceiling on what swapping the\n\
         NLI model could recover. The gate table asks the other question —\n\
         whether the two populations separate by confidence at all, and so\n\
         whether this is fixable without changing the model.",
        r.judgment_share_of_misses() * 100.0
    );
}

fn print_bench(r: &engram_eval::run::BenchReport) {
    let rt = &r.runtime;
    println!("engram-eval — retrieval strategy bench");
    println!(
        "runtime: embedder={}  reranker={}  seed={}  limit={}",
        rt.embedder, rt.reranker, rt.seed, rt.limit
    );
    if rt.embeddings_are_fake {
        println!("!! FAKE EMBEDDINGS — every semantic number below is noise");
    }
    println!(
        "graph {} facts, {} edges / {} questions\n",
        r.graph, r.edges, r.questions
    );
    println!(
        "  {:<32} {:>9} {:>6} {:>6} {:>6} {:>6} {:>6} {:>9} {:>6}",
        "strategy", "weighted", "R@1", "R@5", "lex", "para", "obliq", "tok/query", "sep"
    );
    for row in &r.rows {
        println!(
            "  {:<32} {:>9.3} {:>6.2} {:>6.2} {:>6.2} {:>6.2} {:>6.2} {:>9.0} {:>6.2}",
            row.label,
            row.weighted_recall,
            row.recall_at_1,
            row.recall_at_5,
            row.lexical,
            row.paraphrase,
            row.oblique,
            row.tokens_mean,
            row.separation
        );
    }
    println!(
        "\nThe first two rows are references, not candidates: what a conventional\n\
         vector stack scores, and what Engram ships. Every `bench:` row below them\n\
         reads the same store and the same embeddings, so a difference is a\n\
         difference in ranking strategy and nothing else. Indented rows change one\n\
         thing from the row above the group. `obliq` — recall on questions that\n\
         never name their subject — is the column the shipped stack loses on and\n\
         therefore the only one worth beating rag in."
    );
}

/// `decision=35,caution=20,...` — unknown names are an error rather than a
/// silent zero, since a typo would otherwise remove a whole type's questions
/// and nothing would say so.
fn parse_type_mix(spec: &str) -> anyhow::Result<Vec<(Kind, u32)>> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let (name, weight) = part
            .trim()
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("type-mix entry {part:?} is not name=weight"))?;
        let kind = KINDS
            .iter()
            .find(|k| format!("{k:?}").to_lowercase() == name.trim().to_lowercase())
            .ok_or_else(|| anyhow::anyhow!("unknown node type {name:?} in --type-mix"))?;
        out.push((*kind, weight.trim().parse()?));
    }
    if out.iter().all(|(_, w)| *w == 0) {
        anyhow::bail!("--type-mix leaves every type at zero weight");
    }
    Ok(out)
}

/// One generated fact per kind, in full, plus a distractor and the labelled
/// pairs — so the vocabularies and templates can be reviewed by eye instead of
/// inferred from a score table.
fn print_sample(cfg: &Config) {
    let size = *cfg.sizes.first().expect("checked non-empty");
    let c = corpus(size, size * cfg.distractor_ratio, cfg.seed);
    println!(
        "seed {} — {} tested + {} distractors = {} facts, {} questions, {} pairs, {} controls\n",
        cfg.seed,
        c.tested(),
        c.distractors(),
        c.facts.len(),
        c.questions().count(),
        c.pairs.len(),
        c.unanswerable.len()
    );

    for kind in KINDS {
        let Some(f) = c.facts.iter().find(|f| f.tested && f.kind == kind) else {
            continue;
        };
        println!("── {:?} ({})", f.kind, f.key);
        println!("   title   {}", f.title);
        println!("   body    {}", f.body);
        println!("   answer  {}", f.answer);
        for q in &f.questions {
            println!(
                "   {:<10} {}",
                format!("{:?}", q.phrasing).to_lowercase(),
                q.text
            );
        }
        for p in c.pairs.iter().filter(|p| p.premise == f.title) {
            println!("   {:<10} {}", p.gold.as_str(), p.hypothesis);
        }
        println!();
    }

    if let Some(d) = c.facts.iter().find(|f| !f.tested) {
        println!(
            "── distractor ({}) — written to the graph, never asked about",
            d.key
        );
        println!("   title   {}", d.title);
        println!("   body    {}\n", d.body);
    }

    println!("── controls — subjects that were never written; any answer is a false positive");
    for q in c.unanswerable.iter().take(3) {
        println!(
            "   {:<10} {}",
            format!("{:?}", q.phrasing).to_lowercase(),
            q.text
        );
    }
}

/// The online half's input: every question, with the context each arm would
/// have supplied. Written, never executed — running it costs money and is a
/// deliberate, separate decision.
fn emit_tasks(cfg: &Config, path: &str) -> anyhow::Result<usize> {
    let size = *cfg.sizes.iter().max().expect("checked non-empty");
    let c = corpus(size, size * cfg.distractor_ratio, cfg.seed);
    let whole = WholeFileArm::new(&c);
    let grep = GrepArm::new(&c);
    // The real cortex, not the fake one. These contexts are what a live model
    // will be asked to answer from, so building them with a bag-of-bytes
    // embedder would hand the online half a manifest of noise and grade the
    // model on it.
    let (emb, emb_name) = engram_eval::run::embedder(cfg.embed_model.as_deref());
    if emb_name.contains("(fake)") {
        anyhow::bail!(
            "refusing to emit online tasks under fake embeddings —              the engram arm's context would be noise. Build with --features fastembed."
        );
    }
    let engram = EngramArm::build(&c, emb, engram_eval::run::reranker().0)?;
    // `rag` is the arm engram is actually competing with — a manifest without
    // it can only compare against a flat file and a full dump, neither of
    // which anybody is choosing instead.
    let rag = RagArm::new(
        &engram,
        engram_eval::run::embedder(cfg.embed_model.as_deref()).0,
    );
    let arms: [&dyn Arm; 4] = [&whole, &grep, &rag, &engram];

    let tasks: Vec<online::Task> = arms
        .iter()
        .flat_map(|a| online::tasks(&c, *a, cfg.limit))
        .collect();
    std::fs::write(path, serde_json::to_string_pretty(&tasks)?)?;
    Ok(tasks.len())
}

fn phrasing_recall(arm: &engram_eval::run::ArmReport, p: Phrasing) -> f64 {
    arm.by_phrasing
        .iter()
        .find(|s| s.phrasing == p)
        .map(|s| s.score.recall_at_5)
        .unwrap_or_default()
}

fn print_report(r: &Report) {
    let rt = &r.runtime;
    println!("engram-eval — offline retrieval suite");
    println!(
        "runtime: embedder={}  reranker={}  nli={}  seed={}  limit={}",
        rt.embedder, rt.reranker, rt.nli, rt.seed, rt.limit
    );
    println!(
        "corpus:  bodies ~{} chars, {} edges/node — profiled from the real graph",
        rt.profile.body_chars.median, rt.profile.edges_per_node
    );
    let mix: Vec<String> = rt
        .type_mix
        .iter()
        .map(|(k, w)| format!("{}={w}", format!("{k:?}").to_lowercase()))
        .collect();
    println!(
        "asked:   {} (an assumption, not a measurement)",
        mix.join(" ")
    );
    if rt.embeddings_are_fake {
        println!(
            "\n!! FAKE EMBEDDINGS — the harness works, the semantic numbers do not mean\n\
             !! anything. Rebuild with --features fastembed before quoting a result."
        );
    }

    for s in &r.sizes {
        println!(
            "\ngraph {} facts ({} tested + {} distractors), {} edges / {} questions / {} controls",
            s.graph, s.size, s.distractors, s.edges, s.questions, s.unanswerable
        );
        for c in &s.curated {
            println!(
                "  {} holds {} of {} facts at a {}-token budget ({:.0}% of the graph)",
                c.arm,
                c.held,
                s.graph,
                c.budget,
                100.0 * c.held as f64 / s.graph.max(1) as f64
            );
        }
        println!(
            "  {:<14} {:>8} {:>9} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6}",
            "arm",
            "standing",
            "tok/query",
            "focus",
            "noise",
            "R@1",
            "R@5",
            "MRR",
            "lex",
            "para",
            "obliq",
            "twin",
            "FP",
            "sep",
            "graph"
        );
        for arm in &s.arms {
            println!(
                "  {:<14} {:>8} {:>9.0} {:>6.2} {:>6.2} {:>6.2} {:>6.2} {:>6.2} {:>6.2} {:>6.2} {:>6.2} {:>6.2} {:>6.2} {:>6.2} {:>6.2}",
                arm.arm,
                arm.standing_tokens,
                arm.overall.tokens_mean,
                arm.overall.focus,
                arm.overall.noise,
                arm.overall.recall_at_1,
                arm.overall.recall_at_5,
                arm.overall.mrr,
                phrasing_recall(arm, Phrasing::Lexical),
                phrasing_recall(arm, Phrasing::Paraphrase),
                phrasing_recall(arm, Phrasing::Oblique),
                arm.overall.twin_confusion,
                arm.separation.false_positive_rate,
                arm.separation.balanced_accuracy,
                arm.overall.neighbor_only,
            );
        }

        // The headline: recall as the assumed workload would experience it,
        // plus the oblique share at which the ranking would flip. If that
        // crossover sits near the assumption, the conclusion is an artifact of
        // the assumption and should be quoted as one.
        let named: Vec<(&str, f64)> = s
            .arms
            .iter()
            .map(|a| {
                let split: Vec<_> = a
                    .by_phrasing
                    .iter()
                    .map(|p| (p.phrasing, p.score.clone()))
                    .collect();
                (a.arm.as_str(), rt.phrasing.weighted_recall(&split))
            })
            .collect();
        // Always-in-context arms are excluded: holding a fact is not
        // retrieving it, and at small sizes they hold everything.
        let best = named
            .iter()
            .filter(|(n, _)| *n != "whole-file" && !n.starts_with("curated"))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((winner, wr)) = best {
            print!("  weighted recall@5: ");
            for (n, w) in &named {
                print!("{n} {w:.2}  ");
            }
            println!("\n  best retrieving arm: {winner} at {wr:.3}");
        }
        let split_of = |name: &str| -> Option<Vec<_>> {
            s.arms.iter().find(|a| a.arm == name).map(|a| {
                a.by_phrasing
                    .iter()
                    .map(|p| (p.phrasing, p.score.clone()))
                    .collect()
            })
        };
        if let (Some(e), Some(r)) = (split_of("engram-hybrid"), split_of("rag"))
            && let Some(w) = PhrasingMix::crossover(&e, &r)
        {
            println!(
                "  engram and rag tie at {:.0}% oblique questions (assumed {:.0}%)",
                w * 100.0,
                100.0 * rt.phrasing.oblique
                    / (rt.phrasing.lexical + rt.phrasing.paraphrase + rt.phrasing.oblique)
            );
        }

        let n = &s.nli;
        print!(
            "  nli {} — {} pairs, accuracy {:.2}, mean confidence {:.2}",
            n.model, n.pairs_examined, n.accuracy, n.mean_confidence
        );
        if n.truncated {
            print!(" (budget cut {} of {})", n.pairs_examined, n.pairs_total);
        }
        println!();
        for l in &n.per_label {
            println!(
                "      {:<14} precision {:.2}  recall {:.2}  support {}",
                l.label, l.precision, l.recall, l.support
            );
        }
    }

    println!(
        "\nreading it: `standing` is what the arm costs every session before a question\n\
         is asked; `tok/query` is what it delivers per question. `focus` is the share\n\
         of the delivered tokens that were the answer, when it arrived at all —\n\
         attention is the budget, and everything delivered beyond the answer spends\n\
         it. A dump can score recall 1.00 and focus 0.00x on the same run; that pair\n\
         of numbers is the difference between present and readable. `noise` is the\n\
         share of delivered records that were NOT the answer, counting a miss as\n\
         all-noise and an empty return as zero — the one column where declining to\n\
         answer scores better than guessing. `obliq` is recall on\n\
         questions that never name their subject — the column meaning has to win.\n\
         `FP` is the share of questions with no written answer that still got one;\n\
         `sep` is how well any confidence threshold could have told those apart\n\
         (0.5 = not at all, 1.0 = perfectly). `graph` is the share of questions\n\
         whose answer was reachable ONLY through a neighbour — engram-full minus\n\
         engram-hybrid is what the edges bought."
    );
}
