//! One guarded graph edit on the disposable server from dolt_graph_rust.py.

use hive_dolt_spike::guard::{FixtureWriter, LockEvent};
use hive_dolt_spike::{Target, begin};
use mysql::Transaction;
use mysql::prelude::Queryable;
use std::collections::HashMap;
use std::error::Error;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

type Edge = (String, String);

enum Kind {
    AddPrerequisite,
    Reparent,
}

struct Endpoint<'a> {
    issue: &'a str,
    expected: &'a str,
    revision: &'a str,
}

impl<'a> Endpoint<'a> {
    fn parse(issue: &'a str, expected: &'a str, revision: &'a str) -> Result<Self, &'static str> {
        Ok(Self {
            issue: fixture_id(issue)?,
            expected: fixture_id(expected)?,
            revision: fixture_id(revision)?,
        })
    }
}

fn lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

fn fixture_id(value: &str) -> Result<&str, &'static str> {
    let valid = value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => lower_hex(byte),
        });
    valid
        .then_some(value)
        .ok_or("expected a lowercase fixture UUID")
}

fn observe_lock(event: LockEvent) -> io::Result<()> {
    let mut output = io::stdout().lock();
    match event {
        LockEvent::Busy => writeln!(output, "busy")?,
        LockEvent::Acquired => writeln!(output, "acquired")?,
    }
    output.flush()?;
    if event == LockEvent::Acquired {
        // The Python callback can pause the holder here, before the connection opens.
        let mut reply = [0; 3];
        io::stdin().read_exact(&mut reply)?;
        if &reply != b"go\n" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "missing writer acknowledgement",
            ));
        }
    }
    Ok(())
}

fn has_cycle<'a>(edges: impl IntoIterator<Item = &'a Edge>) -> bool {
    let mut incoming = HashMap::<&str, usize>::new();
    let mut outgoing = HashMap::<&str, Vec<&str>>::new();
    for (source, target) in edges {
        incoming.entry(source).or_default();
        *incoming.entry(target).or_default() += 1;
        outgoing.entry(source).or_default().push(target);
    }

    let mut ready: Vec<_> = incoming
        .iter()
        .filter_map(|(&issue, &count)| (count == 0).then_some(issue))
        .collect();
    let mut visited = 0;
    while let Some(issue) = ready.pop() {
        visited += 1;
        if let Some(targets) = outgoing.get(issue) {
            for &target in targets {
                let count = incoming.get_mut(target).expect("edge target counted");
                *count -= 1;
                if *count == 0 {
                    ready.push(target);
                }
            }
        }
    }
    visited != incoming.len()
}

fn rejection(
    tx: &mut Transaction<'_>,
    kind: &Kind,
    [source, target]: &[Endpoint<'_>; 2],
    check_combined: bool,
) -> Result<Option<&'static str>, Box<dyn Error>> {
    let issues: Vec<(String, Option<String>, String)> =
        tx.query("SELECT id, parent_id, revision FROM issues")?;
    for endpoint in [source, target] {
        if !issues
            .iter()
            .any(|(issue, _, revision)| issue == endpoint.issue && revision == endpoint.expected)
        {
            return Ok(Some("stale-revision"));
        }
    }

    let mut dependencies: Vec<Edge> =
        tx.query("SELECT issue_id, prerequisite_id FROM issue_dependencies")?;
    let mut parents: Vec<Edge> = issues
        .into_iter()
        .filter_map(|(issue, parent, _)| parent.map(|parent| (parent, issue)))
        .collect();
    let edge = (source.issue.to_owned(), target.issue.to_owned());
    match kind {
        Kind::AddPrerequisite => dependencies.push(edge),
        Kind::Reparent => {
            if parents.iter().any(|(_, child)| child == target.issue) {
                return Err("reparent fixture expects the child to be a root".into());
            }
            parents.push(edge);
        }
    }
    Ok(if has_cycle(&dependencies) {
        Some("prerequisite-cycle")
    } else if has_cycle(&parents) {
        Some("parent-cycle")
    } else if check_combined && has_cycle(dependencies.iter().chain(&parents)) {
        Some("completion-cycle")
    } else {
        None
    })
}

fn write_edit(
    tx: &mut Transaction<'_>,
    kind: Kind,
    [source, target]: &[Endpoint<'_>; 2],
    request: &str,
    fingerprint: &str,
    base: &str,
) -> Result<(), Box<dyn Error>> {
    let (relation, name) = match kind {
        Kind::AddPrerequisite => (
            "INSERT INTO issue_dependencies (issue_id, prerequisite_id) VALUES (?, ?)",
            "add-prerequisite",
        ),
        Kind::Reparent => (
            "UPDATE issues SET parent_id = ? WHERE id = ? AND parent_id IS NULL",
            "reparent",
        ),
    };
    tx.exec_drop(relation, (source.issue, target.issue))?;
    if tx.affected_rows() != 1 {
        return Err("expected one graph relation to change".into());
    }
    for endpoint in [source, target] {
        tx.exec_drop(
            "UPDATE issues SET revision = ?, updated_at = UTC_TIMESTAMP(6) WHERE id = ? AND revision = ?",
            (endpoint.revision, endpoint.issue, endpoint.expected),
        )?;
        if tx.affected_rows() != 1 {
            return Err("expected one endpoint revision to change".into());
        }
    }
    // The endpoint values are validated UUIDs before they enter the receipt JSON.
    let result = format!(
        r#"{{"revisions":{{"{}":"{}","{}":"{}"}}}}"#,
        source.issue, source.revision, target.issue, target.revision,
    );
    tx.exec_drop(
        "INSERT INTO operation_receipts
         (request_id, kind, fingerprint, actor_ref, authorization_ref, workflow_evidence_json,
          base_commit, result_json, created_at)
         VALUES (?, ?, ?, 'synthetic-writer', 'synthetic-authorization', '{}', ?, ?, UTC_TIMESTAMP(6))",
        (request, name, fingerprint, base, result),
    )?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [
        lock,
        kind,
        combined,
        request,
        fingerprint,
        source,
        source_expected,
        source_revision,
        target,
        target_expected,
        target_revision,
    ] = args.as_slice()
    else {
        return Err("expected lock, operation, combined flag, request, fingerprint and two endpoint revision triples".into());
    };
    let kind = match kind.as_str() {
        "add-prerequisite" => Kind::AddPrerequisite,
        "reparent" => Kind::Reparent,
        _ => return Err("unknown graph edit".into()),
    };
    let check_combined = combined.parse::<bool>()?;
    let request = fixture_id(request)?;
    if fingerprint.len() != 64 || !fingerprint.bytes().all(lower_hex) {
        return Err("expected a SHA-256 fixture fingerprint".into());
    }
    let endpoints = [
        Endpoint::parse(source, source_expected, source_revision)?,
        Endpoint::parse(target, target_expected, target_revision)?,
    ];

    let target = Target::from_fixture_env()?;
    let mut writer = FixtureWriter::acquire(
        &target,
        Path::new(lock),
        Duration::from_secs(20),
        observe_lock,
    )?;
    let mut tx = begin(&mut writer.connection, true)?;
    let base: String = tx
        .query_first("SELECT DOLT_HASHOF('HEAD')")?
        .ok_or("missing base commit")?;
    if let Some(reason) = rejection(&mut tx, &kind, &endpoints, check_combined)? {
        tx.rollback()?;
        writeln!(io::stdout().lock(), "{reason} {base}")?;
        return Ok(());
    }
    write_edit(&mut tx, kind, &endpoints, request, fingerprint, &base)?;
    tx.commit()?;

    let (commit, parent): (String, String) = writer
        .connection
        .query_first("SELECT DOLT_HASHOF('HEAD'), DOLT_HASHOF('HEAD~1')")?
        .ok_or("missing committed history")?;
    let receipt: Option<String> = writer.connection.exec_first(
        "SELECT base_commit FROM operation_receipts AS OF 'HEAD' WHERE request_id = ?",
        (request,),
    )?;
    if commit == base || parent != base || receipt.as_deref() != Some(base.as_str()) {
        return Err("expected a direct successor commit with the graph receipt".into());
    }
    writeln!(io::stdout().lock(), "committed {base} {commit}")?;
    Ok(())
}
