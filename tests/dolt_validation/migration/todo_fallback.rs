use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

use super::{
    AREA_GENERAL, AREA_USERDATA, EARLIER_POLICY_PIN, POLICY_PIN, git, git_text, id, init_git,
    migration_cli_at, sha256_hex,
};

const V2_POLICY: &str = "c66b3c50150e18426485d134de92c6b311dac125";
const TITLE: &str = "Synthetic private Task title";
const CRITERIA: &str = "Verify the synthetic fixture has no mismatches.";
const CREATED: &str = "2026-04-01T09:00:00Z";
const UPDATED: &str = "2026-04-02T09:00:00Z";
const CLOSED: &str = "2026-04-03T09:00:00Z";
const ACTOR: &str = "synthetic:reviewer";
const RESOLUTION: &str = "Retained synthetic closure evidence.";

#[test]
fn reviews_todo_backlog_fallback_on_both_pins() {
    for pin in [POLICY_PIN, EARLIER_POLICY_PIN] {
        check_fallback(Path::new(env!("CARGO_BIN_EXE_hive")), pin, false);
    }
}

#[test]
#[ignore = "requires HIVE_STAGING_INSTALLED_BIN, pinned Dolt at HIVE_DOLT_BIN, and native Git"]
fn installed_todo_fallback_stages_backlog_with_exact_numbers_on_both_pins() {
    let binary = std::env::var_os("HIVE_STAGING_INSTALLED_BIN").unwrap();
    for pin in [POLICY_PIN, EARLIER_POLICY_PIN] {
        check_fallback(Path::new(&binary), pin, true);
    }
}

fn check_fallback(binary: &Path, pin: &str, installed: bool) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let source = temp.path().join("source");
    let config = temp.path().join("config");
    let target = temp.path().join("inactive");
    let manifest = temp.path().join("review.json");
    fs::create_dir(&home).unwrap();
    init_git(
        &source,
        &home,
        "Synthetic reviewer",
        "operator@sample.invalid",
    );
    fs::write(source.join("AGENTS.md"), format!("hive-system@{pin}\n")).unwrap();
    if pin == POLICY_PIN {
        fs::write(
            source.join("hive.yml"),
            "version: 1\ncoordination:\n  mode: local\n",
        )
        .unwrap();
    }
    let todo = "status: todo\n";
    let fixtures = [
        (
            "a-reviewed",
            "active",
            todo,
            "```text\nSynthetic example output\n```\n",
        ),
        (
            "b-tagged",
            "active",
            "status: todo\ntags: [synthetic-label]\n",
            "",
        ),
        ("graph-hold", "active", todo, ""),
        ("missing-relations", "active", todo, ""),
        ("unresolved-relations", "active", todo, ""),
        ("default-hold", "active", todo, ""),
        (
            "available-body",
            "active",
            todo,
            "## Acceptance\n\nThe result must be reproducible.\n",
        ),
        (
            "formatted-definition",
            "active",
            todo,
            "## Definition of **Done**\n\nThe output must be reproducible.\n",
        ),
        (
            "nbsp-acceptance",
            "active",
            todo,
            "## Acceptance\u{00a0}Criteria\n\nThe output must be reproducible.\n",
        ),
        (
            "formatted-paragraph",
            "active",
            todo,
            "The **completion** criteria are listed below.\n\nThe output must be reproducible.\n",
        ),
        (
            "inline-code-marker",
            "active",
            todo,
            "`Completion Criteria`:\n\nThe output must be reproducible.\n",
        ),
        (
            "fenced-marker",
            "active",
            todo,
            "```markdown\n## Definition of Done\nThe output must be reproducible.\n```\n",
        ),
        (
            "available-checklist",
            "active",
            todo,
            "- [ ] Verify the outputs.\n",
        ),
        ("available-range", "active", todo, CRITERIA),
        ("invalid-range", "active", todo, ""),
        ("null-range", "active", todo, ""),
        ("missing-dates", "active", todo, ""),
        (
            "claimed",
            "active",
            "status: todo\nclaim: {id: synthetic-claim, holder: synthetic-holder}\n",
            "",
        ),
        ("review", "active", "status: review\n", ""),
        ("wrong-area", "active", todo, ""),
        ("wrong-blob", "active", todo, ""),
        ("original-backlog", "backlog", "", ""),
        ("already-done", "archive", "status: done\n", ""),
        ("already-cancelled", "archive", "status: cancelled\n", ""),
        ("z-regular", "active", todo, CRITERIA),
    ];
    let body_for = |lifecycle, extra| {
        let mut body = format!("# {TITLE}\n\nCreated: {CREATED}\nUpdated: {UPDATED}\n\n{extra}");
        if lifecycle == "archive" {
            body.push_str(&format!("Closed: {CLOSED}\nActor: {ACTOR}\n{RESOLUTION}\n"));
        }
        body
    };
    for (name, lifecycle, fields, extra) in fixtures {
        let path = source.join(format!("tasks/_general/{lifecycle}/{name}/README.md"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = body_for(lifecycle, extra);
        fs::write(
            path,
            format!("---\ndescription: Synthetic Task\n{fields}---\n{body}"),
        )
        .unwrap();
    }
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic todo backlog decisions"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let index = fs::read(source.join(".git/index")).unwrap();
    let area = id(AREA_GENERAL);
    let workspace = id(1);
    let key = |name| json!({"scope":"_general", "work_id":name});
    let fallback = json!({"decision":"backlog_without_criteria"});
    let mut reviews = Vec::new();
    for (name, lifecycle, _, extra) in fixtures {
        let body = body_for(lifecycle, extra);
        let range = |value: &str| {
            let start = body.find(value).unwrap();
            json!({"start": start, "end": start + value.len()})
        };
        let oid = git_text(
            &source,
            &home,
            &[
                "rev-parse",
                &format!("HEAD:tasks/_general/{lifecycle}/{name}/README.md"),
            ],
        );
        let mut review = json!({
            "legacy_key":key(name), "source_git_blob_sha":oid, "area_id":area,
            "created_at":range(CREATED), "updated_at":range(UPDATED), "relations":"no_edges",
        });
        if !matches!(name, "default-hold" | "z-regular") {
            review["todo_fallback"] = fallback.clone();
        }
        if lifecycle == "archive" {
            review["relations"] = json!({"parent":null, "children":[], "prerequisites":[]});
            review["closure"] = json!({"closed_at":range(CLOSED), "closed_by_ref":range(ACTOR), "resolution_note":range(RESOLUTION)});
        }
        match name {
            "available-range" | "z-regular" => review["acceptance_criteria"] = range(CRITERIA),
            "invalid-range" => review["acceptance_criteria"] = json!({"start":9000,"end":9001}),
            "null-range" => review["acceptance_criteria"] = Value::Null,
            "missing-dates" => review["created_at"] = Value::Null,
            "wrong-area" => review["area_id"] = json!(id(AREA_USERDATA)),
            "wrong-blob" => review["source_git_blob_sha"] = json!("e".repeat(oid.len())),
            "missing-relations" => {
                review.as_object_mut().unwrap().remove("relations");
            }
            "unresolved-relations" => review["relations"] = json!("unresolved"),
            _ => {}
        }
        let prerequisites = match name {
            "a-reviewed" => Some(vec![key("z-regular")]),
            "graph-hold" => Some(vec![key("missing-prerequisite")]),
            "z-regular" => Some(vec![]),
            _ => None,
        };
        if let Some(prerequisites) = prerequisites {
            review["relations"] =
                json!({"parent":null, "children":[], "prerequisites":prerequisites});
        }
        reviews.push(review);
    }
    let mapping = json!({
        "version":1, "source_commit":commit, "policy_pin":pin, "workspace_id":workspace,
        "scopes":[{"scope":"_general","decision":{"kind":"area","area":{"id":area,"slug":"general","name":"General"}}}],
        "task_reviews":reviews,
    });
    let path = std::env::var_os("PATH").unwrap();
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
        "--mapping-file",
        manifest.to_str().unwrap(),
        "--workspace-id",
        &workspace,
        "--tasks",
    ];
    let invoke = |action, args: &[&str], input: &Value, exit| {
        fs::write(&manifest, serde_json::to_vec(input).unwrap()).unwrap();
        let result = migration_cli_at(binary, &home, &config, action, args, exit, &path);
        let output = result.to_string();
        for private in [
            TITLE,
            CRITERIA,
            ACTOR,
            RESOLUTION,
            "synthetic-holder",
            "synthetic-label",
        ] {
            assert!(!output.contains(private));
        }
        result
    };
    let report = invoke("migration-plan", &args, &mapping, 0);
    let intake = &report["data"]["task_intake"];
    assert_eq!(
        intake["counts"],
        json!({"inventoried":fixtures.len(),"prepared":2,"held":fixtures.len()-2,"excluded":0})
    );
    let outcome = |name| {
        &intake["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["legacy_key"] == key(name))
            .unwrap()["outcome"]
    };
    assert_eq!(outcome("a-reviewed")["candidate_status"], "backlog");
    assert_eq!(
        outcome("a-reviewed")["provenance"]["todo_fallback"],
        fallback
    );
    assert!(
        outcome("a-reviewed")["provenance"]
            .get("acceptance_criteria")
            .is_none()
    );
    assert_eq!(outcome("z-regular")["candidate_status"], "todo");
    assert!(
        outcome("z-regular")["provenance"]
            .get("todo_fallback")
            .is_none()
    );
    for (name, reason) in [
        ("default-hold", "unverified_acceptance_criteria"),
        ("b-tagged", "unverified_task_labels"),
        ("graph-hold", "unresolved_task_relations"),
        ("missing-relations", "unresolved_task_relations"),
        ("unresolved-relations", "unresolved_task_relations"),
        ("available-body", "inapplicable_todo_fallback"),
        ("formatted-definition", "inapplicable_todo_fallback"),
        ("nbsp-acceptance", "inapplicable_todo_fallback"),
        ("formatted-paragraph", "inapplicable_todo_fallback"),
        ("inline-code-marker", "inapplicable_todo_fallback"),
        ("fenced-marker", "inapplicable_todo_fallback"),
        ("available-checklist", "inapplicable_todo_fallback"),
        ("available-range", "inapplicable_todo_fallback"),
        ("invalid-range", "inapplicable_todo_fallback"),
        ("null-range", "inapplicable_todo_fallback"),
        ("missing-dates", "incomplete_historical_provenance"),
        (
            "claimed",
            if pin == POLICY_PIN {
                "execution_needs_reconciliation"
            } else {
                "invalid_legacy_state"
            },
        ),
        ("review", "missing_typed_submission_evidence"),
        ("wrong-area", "task_review_binding_mismatch"),
        ("wrong-blob", "task_review_binding_mismatch"),
        ("original-backlog", "inapplicable_todo_fallback"),
        ("already-done", "inapplicable_todo_fallback"),
        ("already-cancelled", "inapplicable_todo_fallback"),
    ] {
        let row = outcome(name);
        assert_eq!(row["status"], "held", "{name}");
        assert!(
            row["reason_codes"]
                .as_array()
                .unwrap()
                .contains(&json!(reason)),
            "{name}: {row}"
        );
    }

    for (field, malformed) in [
        ("todo_fallback", json!(["backlog_without_criteria"])),
        ("todo_fallback", Value::Null),
        ("acceptance_criteria", json!([0, 10])),
    ] {
        let mut bad = mapping.clone();
        bad["task_reviews"][0][field] = malformed;
        assert_eq!(
            invoke("migration-plan", &args, &bad, 2)["error"]["code"],
            "INVALID_CONFIG"
        );
    }

    let mut simple = mapping.clone();
    simple["task_reviews"][0]["relations"] = json!("no_edges");
    let control = invoke("migration-plan", &args, &simple, 0);
    let simple_task = control["data"]["task_intake"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["legacy_key"] == key("a-reviewed"))
        .unwrap();
    assert_eq!(simple_task["outcome"]["status"], "prepared");
    assert_eq!(
        simple_task["outcome"]["provenance"]["relations"],
        "no_edges"
    );

    let mut stage_args = args.to_vec();
    stage_args.extend([
        "--target",
        target.to_str().unwrap(),
        "--policy-revision",
        V2_POLICY,
        "--mode",
        "staged-import",
    ]);
    for malformed in [
        json!({"no_edges":null}),
        json!({"unresolved":null}),
        json!([null, [], []]),
        json!("No_Edges"),
    ] {
        let mut bad = mapping.clone();
        bad["task_reviews"][0]["relations"] = malformed;
        assert_eq!(
            invoke("migration-plan", &args, &bad, 2)["error"]["code"],
            "INVALID_CONFIG"
        );
        if installed {
            assert_eq!(
                invoke("migration-stage", &stage_args, &bad, 2)["error"]["code"],
                "INVALID_CONFIG"
            );
            assert!(!target.exists());
        }
    }

    if installed {
        let report = invoke("migration-stage", &stage_args, &mapping, 0);
        let data = &report["data"];
        assert_eq!(data["counts"]["tasks"]["imported"], 3);
        assert_eq!(data["counts"]["tasks"]["held"], fixtures.len() - 3);
        for name in ["missing-relations", "unresolved-relations"] {
            let digest = sha256_hex(format!("tasks/_general/active/{name}/README.md").as_bytes());
            let hold = data["holds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["source_path_sha256"] == digest)
                .unwrap();
            assert_eq!(hold["reason_codes"], json!(["unresolved_task_relations"]));
        }
        assert_eq!(data["target"]["issue_number_next_value"], 4);
        assert_eq!(data["target"]["active_claims"], 0);
        assert_eq!(data["checks"]["published_inactive"], true);
        let task_rows: Vec<_> = data["id_map"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["kind"] == "task")
            .collect();
        assert_eq!(task_rows.len(), 3);
        for name in [
            "formatted-definition",
            "nbsp-acceptance",
            "formatted-paragraph",
        ] {
            let digest = sha256_hex(format!("tasks/_general/active/{name}/README.md").as_bytes());
            let hold = data["holds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["source_path_sha256"] == digest)
                .unwrap();
            assert!(
                hold["reason_codes"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("inapplicable_todo_fallback"))
            );
            assert!(
                task_rows
                    .iter()
                    .all(|row| row["source_path_sha256"] != digest)
            );
        }
        for (offset, (name, status)) in [
            ("a-reviewed", "backlog"),
            ("b-tagged", "backlog"),
            ("z-regular", "todo"),
        ]
        .into_iter()
        .enumerate()
        {
            let row = task_rows[offset];
            assert_eq!(
                row["source_path_sha256"],
                sha256_hex(format!("tasks/_general/active/{name}/README.md").as_bytes())
            );
            assert_eq!(row["issue_number"], offset + 1);
            assert_eq!(row["target_status"], status);
            if status == "backlog" {
                assert_eq!(row["todo_fallback"], fallback);
            } else {
                assert!(row.get("todo_fallback").is_none());
            }
        }
        verify_committed(&home, &target.join("db"), task_rows.as_slice());
    }
    assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    assert_eq!(fs::read(source.join(".git/index")).unwrap(), index);
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
}

fn verify_committed(home: &Path, db: &Path, task_rows: &[&Value]) {
    let dolt = std::env::var_os("HIVE_DOLT_BIN").unwrap();
    let query = |sql: &str| {
        let output = Command::new(&dolt)
            .env_clear()
            .env("HOME", home)
            .env("DOLT_ROOT_PATH", home)
            .env("DOLT_DISABLE_VERSION_CHECKS", "true")
            .env("DOLT_DISABLE_EVENT_FLUSH", "true")
            .current_dir(db)
            .args(["sql", "-r", "json", "-q", sql])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["rows"].clone()
    };
    let rows = query(
        "SELECT id, number, status, SHA2(CAST(acceptance_criteria AS BINARY), 256) AS criteria FROM issues AS OF 'HEAD' ORDER BY number",
    );
    assert_eq!(
        rows,
        json!([
            {"id":task_rows[0]["issue_id"], "number":1, "status":"backlog"},
            {"id":task_rows[1]["issue_id"], "number":2, "status":"backlog"},
            {"id":task_rows[2]["issue_id"], "number":3, "status":"todo", "criteria":sha256_hex(CRITERIA.as_bytes())}
        ])
    );
    assert_eq!(
        query("SELECT issue_id, prerequisite_id FROM issue_dependencies AS OF 'HEAD'"),
        json!([{"issue_id":task_rows[0]["issue_id"],"prerequisite_id":task_rows[2]["issue_id"]}])
    );
    assert_eq!(
        query("SELECT issue_id FROM issue_labels AS OF 'HEAD'"),
        json!([{"issue_id":task_rows[1]["issue_id"]}])
    );
}
