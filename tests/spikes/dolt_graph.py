#!/usr/bin/env python3
"""Run Issue #26's synthetic graph probe with python3 -B tests/spikes/dolt_graph.py.

Set HIVE_DOLT_BIN to the directory containing the verified Dolt 2.7.6 binary.
run_suite(dolt, writer=...) accepts a fixture writer in place of the CLI adapter.
"""

import hashlib
import json
import multiprocessing
import os
from contextlib import contextmanager
from dataclasses import dataclass
from graphlib import CycleError, TopologicalSorter
from pathlib import Path

from dolt_graph_fixture import VERSION, WAIT_SECONDS, open_fixture, writer_lock

PREREQUISITE = "add-prerequisite"
PARENT = "reparent"


def synthetic_id(number):
    return f"01900000-0000-7000-8000-{number:012d}"


ISSUES = A, B, C, D = tuple(synthetic_id(number) for number in range(1, 5))
INITIAL_REVISIONS = {
    issue: synthetic_id(index) for index, issue in enumerate(ISSUES, 11)
}
RACE_DEPENDENCIES = {(B, C), (D, A)}


@dataclass(frozen=True)
class Edit:
    kind: str
    edge: tuple[str, str]  # issue -> prerequisite or parent -> child
    request_id: str
    expected_revisions: dict[str, str]
    revisions: dict[str, str]


def edit(kind, edge, number):
    return Edit(
        kind,
        edge,
        synthetic_id(number),
        {issue: INITIAL_REVISIONS[issue] for issue in edge},
        {issue: synthetic_id(index) for index, issue in enumerate(edge, number * 10)},
    )


ADD = edit(PREREQUISITE, (A, B), 21)
MOVE = edit(PARENT, (C, D), 22)


def seed_graph(dependencies, parents):
    area = synthetic_id(10)
    issues = ",\n".join(
        f"('{issue}', {number}, '{area}', '{name}', 'task', 'todo', "
        f"'{INITIAL_REVISIONS[issue]}', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))"
        for number, (issue, name) in enumerate(zip(ISSUES, "ABCD", strict=True), 1)
    )
    statements = [
        (
            "INSERT INTO areas (id, slug, name, revision) VALUES "
            f"('{area}', 'graph', 'Synthetic Graph', '{synthetic_id(15)}');"
        ),
        (
            "INSERT INTO issues (id, number, area_id, title, type, status, revision, "
            f"created_at, updated_at) VALUES {issues};"
        ),
    ]
    statements.extend(
        f"INSERT INTO issue_dependencies (issue_id, prerequisite_id) VALUES ('{issue}', '{prereq}');"
        for issue, prereq in sorted(dependencies)
    )
    statements.extend(
        f"UPDATE issues SET parent_id='{parent}' WHERE id='{child}';"
        for parent, child in sorted(parents)
    )
    return "\n".join(statements)


@dataclass(frozen=True)
class Outcome:
    status: str
    base: str
    commit: str | None = None


def cycle_in(dependencies, parents, check_combined):
    checks = [("prerequisite-cycle", dependencies), ("parent-cycle", parents)]
    if check_combined:
        checks.append(("completion-cycle", dependencies | parents))
    for reason, edges in checks:
        sorter = TopologicalSorter()
        for issue, obligation in edges:
            sorter.add(issue, obligation)
        try:
            sorter.prepare()
        except CycleError:
            return reason
    return None


def commit_edit(fixture, change, base):
    source, target = change.edge
    if change.kind == PREREQUISITE:
        relation = f"INSERT INTO issue_dependencies VALUES ('{source}', '{target}');"
    else:
        relation = f"UPDATE issues SET parent_id='{source}' WHERE id='{target}';"
    revisions = "\n".join(
        f"UPDATE issues SET revision='{revision}', updated_at=UTC_TIMESTAMP(6) WHERE id='{issue}';"
        for issue, revision in change.revisions.items()
    )
    payload = json.dumps(
        [change.kind, change.edge, change.expected_revisions], sort_keys=True
    )
    fingerprint = hashlib.sha256(payload.encode()).hexdigest()
    result = json.dumps({"revisions": change.revisions}, sort_keys=True)
    fixture.execute(f"""
        SET @@SESSION.dolt_force_transaction_commit=0;
        SET @@SESSION.dolt_transaction_commit=1;
        START TRANSACTION;
        {relation}
        {revisions}
        INSERT INTO operation_receipts
            (request_id, kind, fingerprint, actor_ref, authorization_ref, workflow_evidence_json,
             base_commit, result_json, created_at)
        VALUES ('{change.request_id}', '{change.kind}', '{fingerprint}', 'synthetic-writer',
                'synthetic-authorization', '{{}}', '{base}', '{result}', UTC_TIMESTAMP(6));
        COMMIT;
    """)
    parent, commit = fixture.history(1)
    assert parent == base and commit != base, (base, parent, commit)
    receipts = fixture.rows(f"""
        SELECT base_commit FROM operation_receipts AS OF '{commit}'
        WHERE request_id='{change.request_id}'
    """)
    assert receipts == [(base,)], receipts
    return commit


def guarded_write(fixture, change, *, check_combined=True, on_lock=None):
    """CLI adapter. Hold the guard from the first graph read through confirmation.

    The fixture exposes its root, port, workspace_id and lock path. The
    dolt_graph_fixture module defines the HOST and DATABASE constants.
    An injected writer returns Outcome and calls on_lock (when supplied) with
    'busy' on initial kernel contention and 'acquired' before reading under
    the guard. check_combined=False omits the union check for the control.
    """
    with writer_lock(fixture.lock, on_lock):
        (base,) = fixture.history(0)
        rows = fixture.rows("SELECT id, COALESCE(parent_id, ''), revision FROM issues")
        revisions = {issue: revision for issue, _, revision in rows}
        if any(
            revisions.get(issue) != rev
            for issue, rev in change.expected_revisions.items()
        ):
            return Outcome("stale-revision", base)

        dependencies = set(
            fixture.rows("SELECT issue_id, prerequisite_id FROM issue_dependencies")
        )
        parents = {(parent, issue) for issue, parent, _ in rows if parent}
        if change.kind == PREREQUISITE:
            dependencies.add(change.edge)
        else:
            assert change.kind == PARENT, change.kind
            # The moved child is a root in these fixtures, so only the new endpoints change.
            assert all(child != change.edge[1] for _, child in parents), parents
            parents.add(change.edge)
        reason = cycle_in(dependencies, parents, check_combined)
        if reason:
            return Outcome(reason, base)
        return Outcome("committed", base, commit_edit(fixture, change, base))


def receive(channel):
    if not channel.poll(WAIT_SECONDS):
        raise TimeoutError("Timed out waiting for graph worker")
    return channel.recv()


def expect_event(channel, expected):
    received = receive(channel)
    assert received == expected, (expected, received)


@contextmanager
def writer_process(fixture, change, writer, check_combined, *, pause=False):
    context = multiprocessing.get_context("fork")
    parent, child = context.Pipe()

    def worker():
        parent.close()

        def on_lock(event):
            child.send(event)
            if pause and event == "acquired":
                expect_event(child, "release")

        with child:
            child.send(
                writer(fixture, change, check_combined=check_combined, on_lock=on_lock)
            )

    with parent, child:
        process = context.Process(target=worker)
        process.start()
        child.close()
        try:
            yield parent
            process.join(WAIT_SECONDS)
            assert process.exitcode == 0, process.exitcode
        finally:
            if process.is_alive():
                process.kill()
                process.join(WAIT_SECONDS)
            process.close()


def run_contention(
    fixture, first, second, *, writer=guarded_write, check_combined=True
):
    """Require both processes to overlap at the fixture lock in the chosen order."""
    with writer_process(fixture, first, writer, check_combined, pause=True) as holder:
        expect_event(holder, "acquired")
        with writer_process(fixture, second, writer, check_combined) as contender:
            expect_event(contender, "busy")
            holder.send("release")
            expect_event(contender, "acquired")
            return receive(holder), receive(contender)


def cyclic_issues(edges):
    # Independent of the writer's topological sort. Close paths through each Issue.
    reach = set(edges)
    for issue in ISSUES:
        incoming = {source for source, target in reach if target == issue}
        outgoing = {target for source, target in reach if source == issue}
        reach.update((source, target) for source in incoming for target in outgoing)
    return {issue for issue in ISSUES if (issue, issue) in reach}


def verify_history(fixture, dependencies, parents, accepted, *, cycle_expected=False):
    """Read exact commits through fresh CLI connections using only the scenario's expectations."""
    history = fixture.history(len(accepted))
    assert history[0] == fixture.base, history
    assert fixture.rows("SELECT table_name, staged, status FROM dolt_status") == []
    expected_dependencies = set(dependencies)
    expected_parents = set(parents)
    expected_revisions = INITIAL_REVISIONS.copy()
    expected_receipts = {}

    for index, commit in enumerate(history):
        if index:
            change = accepted[index - 1]
            edges = (
                expected_dependencies
                if change.kind == PREREQUISITE
                else expected_parents
            )
            edges.add(change.edge)
            expected_revisions.update(change.revisions)
            expected_receipts[change.request_id] = (
                change.kind,
                history[index - 1],
                {"revisions": change.revisions},
            )

        dependencies_at_commit = set(
            fixture.rows(f"""
            SELECT issue_id, prerequisite_id FROM issue_dependencies AS OF '{commit}'
        """)
        )
        parents_at_commit = set(
            fixture.rows(f"""
            SELECT parent_id, id FROM issues AS OF '{commit}' WHERE parent_id IS NOT NULL
        """)
        )
        revisions_at_commit = dict(
            fixture.rows(f"SELECT id, revision FROM issues AS OF '{commit}'")
        )
        receipts_at_commit = {
            request_id: (kind, base, json.loads(result))
            for request_id, kind, base, result in fixture.rows(f"""
                SELECT request_id, kind, base_commit, result_json
                FROM operation_receipts AS OF '{commit}'
            """)
        }
        assert dependencies_at_commit == expected_dependencies, (
            commit,
            dependencies_at_commit,
        )
        assert parents_at_commit == expected_parents, (commit, parents_at_commit)
        assert revisions_at_commit == expected_revisions, (commit, revisions_at_commit)
        assert receipts_at_commit == expected_receipts, (commit, receipts_at_commit)

        assert not cyclic_issues(dependencies_at_commit), (
            commit,
            dependencies_at_commit,
        )
        assert not cyclic_issues(parents_at_commit), (commit, parents_at_commit)
        cycle = cyclic_issues(dependencies_at_commit | parents_at_commit)
        expected_cycle = (
            set(ISSUES) if cycle_expected and index == len(accepted) else set()
        )
        assert cycle == expected_cycle, (commit, cycle)
    return history


def run_suite(dolt, writer=guarded_write):
    if not __debug__:
        raise RuntimeError("Run the graph probe with Python assertions enabled")
    dolt = Path(dolt).resolve(strict=True)
    print(f"Testing Dolt {VERSION} on a disposable loopback server", flush=True)
    assert ADD.expected_revisions.keys().isdisjoint(MOVE.expected_revisions)

    path = {(A, B), (B, C)}
    for kind, dependencies, parents, rejection in (
        (PREREQUISITE, path, set(), "prerequisite-cycle"),
        (PARENT, set(), path, "parent-cycle"),
    ):
        with open_fixture(dolt, seed_graph(dependencies, parents)) as fixture:
            outcome = writer(fixture, edit(kind, (C, A), 23))
            assert outcome == Outcome(rejection, fixture.base), outcome
            verify_history(fixture, dependencies, parents, ())
            print(
                f"PASS {rejection}: A -> B -> C -> A rejected; HEAD {fixture.base}",
                flush=True,
            )

    race_seed = seed_graph(RACE_DEPENDENCIES, set())
    for change in (ADD, MOVE):
        with open_fixture(dolt, race_seed) as fixture:
            outcome = writer(fixture, change)
            base, commit = verify_history(fixture, RACE_DEPENDENCIES, set(), (change,))
            assert outcome == Outcome("committed", base, commit), outcome
            print(f"PASS {change.kind} alone: {base} -> {commit}", flush=True)
            if change == MOVE:
                mixed = writer(fixture, ADD)
                assert mixed == Outcome("completion-cycle", commit), mixed
                history = verify_history(fixture, RACE_DEPENDENCIES, set(), (MOVE,))
                assert history[-1] == commit, history
                print(
                    f"PASS completion-cycle: A -> B -> C -> D -> A rejected; HEAD {commit}",
                    flush=True,
                )

    for first, second in ((ADD, MOVE), (MOVE, ADD)):
        order = f"{first.kind} then {second.kind}"
        with open_fixture(dolt, race_seed) as fixture:
            winner, loser = run_contention(fixture, first, second, writer=writer)
            base, commit = verify_history(fixture, RACE_DEPENDENCIES, set(), (first,))
            assert winner == Outcome("committed", base, commit), winner
            assert loser == Outcome("completion-cycle", commit), loser
            print(
                f"PASS contention {order}: {base} -> {commit}; loser completion-cycle",
                flush=True,
            )

        with open_fixture(dolt, race_seed) as fixture:
            first_result, second_result = run_contention(
                fixture, first, second, writer=writer, check_combined=False
            )
            base, intermediate, final = verify_history(
                fixture, RACE_DEPENDENCIES, set(), (first, second), cycle_expected=True
            )
            assert first_result == Outcome("committed", base, intermediate), (
                first_result
            )
            assert second_result == Outcome("committed", intermediate, final), (
                second_result
            )
            print(
                f"PASS negative control {order}: {base} -> {intermediate} -> {final}; "
                "readback found A -> B -> C -> D -> A",
                flush=True,
            )


if __name__ == "__main__":
    directory = os.environ.get("HIVE_DOLT_BIN")
    if not directory:
        raise SystemExit(
            "Set HIVE_DOLT_BIN to the verified Dolt 2.7.6 binary directory"
        )
    run_suite(Path(directory) / "dolt")
