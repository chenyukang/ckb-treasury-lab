# CKB DAO Treasury Implementation Plan

Last updated: 2026-08-27

## DAO Deposit Age Validation (2026-08-27)

### Objective

Require every DAO deposit referenced by a VoteEventCell to have been created in
a block strictly earlier than the block that created the Proposal Cell. The
Vote transaction supplies both creation headers as HeaderDeps; the Vote Type
Script loads the headers through the resolved CellDeps and compares their block
numbers. No raw creation transaction or transaction-position proof is needed.

### Steps

- [x] Verify that CKB exposes a CellDep's creation header when its block hash is
  present in the transaction's HeaderDeps.
- [x] Add the strict DAO creation block check to the Vote Type Script.
- [x] Add contract tests for older, same-block, newer, and missing-header DAO
  deposits.
- [x] Update the live-chain E2E transaction builder and add a rejected late-DAO
  vote before the successful voting and settlement flow.
- [x] Rebuild all contract binaries and run formatting, Clippy, and the full
  Rust/contract test suite.
- [x] Deploy the rebuilt binaries to a fresh local CKB chain and rerun the full
  live-chain E2E.

### Validation

- The clean release build, formatting check, full Clippy run, 39 standard
  Rust/CKB-VM tests, and the ignored cycle benchmark passed.
- The rebuilt Vote Type Script is 72,544 bytes and has CKB data hash
  `0x15ab76603782b78b5600b9874754c000a081dc9c9e6d9178ffaf1a946b545020`.
- A fresh local CKB chain deployed the rebuilt binaries as genesis system cells.
  DAO deposits from blocks 20 and 24 could vote on the Proposal from block 28;
  a DAO deposit from block 32 was rejected with Vote script error code 20.
- The remaining omitted-vote challenge, complete tally, finalization, and
  Treasury payout flow passed. Report:
  `impl/target/live-e2e/1787804454-33060/report.json`.

## Objective

Replace raw historical vote transactions in tally witnesses with live,
canonical VoteEventCells authenticated by transaction-position proofs. Keep
DAO-spend raw transactions as tally events so a deposit spent during the
voting window still revokes the related vote. Authorization remains delegated
to each DAO deposit's lock script.

## Steps

- [x] Define the V5 hybrid wire model for compact VoteEventCell proofs, raw DAO
  spend events, and the current immutable VoteEventCell lifecycle rule.
- [x] Update the Vote Type Script to validate lock-agnostic authorization,
  eligible DAO deposits, canonical event commitments, and event-cell lifetime.
- [x] Update the Tally Type Script to load VoteEventCells from direct CellDeps,
  authenticate their creation positions against HeaderDeps, while retaining
  raw DAO-spend processing and ChallengeSpend.
- [x] Update the off-chain scanner, batch builder, candidate replay, RPC data
  sources, and live-chain transaction assembly.
- [x] Add positive and adversarial tests, update protocol documentation and
  benchmarks, then run formatting, Clippy, contract tests, cycle benchmarks,
  and the live CKB E2E. Compare V4 and V5 witness/transaction bytes, contract
  binary sizes, and cycles for the same vote counts.

## Implementation Notes

- This is a prototype wire-format upgrade. Tally witnesses move from V4 to V5;
  old witness bytes are rejected instead of being interpreted under new rules.
- A transaction has no single OutPoint. Each VoteEventCell is identified by
  `(vote_tx_hash, output_index)`, while `tx_index` separately identifies the
  vote transaction's position inside a block.
- VoteEventCells are referenced as direct CellDeps so parallel TallyChains do
  not consume or contend on them.
- VoteEventCells are immutable in V5. Safe reclaim remains unresolved because a
  fixed timeout can expire before a delayed tally/challenge completes.
- The tally contract must never trust a builder-supplied event. It checks the
  live CellDep, its configured Vote Type Script, canonical event data, voting
  window, and CBMT inclusion under the referenced block header.
- Vote creation no longer places the whole vote transaction in a tally witness.
  DAO-spend events still carry raw transactions because the reducer must inspect
  their inputs and revoke any VoteRecord backed by a spent DAO deposit.
- This removes the oversized-vote-transaction witness attack. An oversized
  DAO-spend transaction remains a separate residual risk and must be bounded or
  handled without weakening deposit-liveness semantics.

## Progress Log

- 2026-08-21: Inspected the current V4 raw-transaction witness path, Vote Cell
  creation, DAO-spend removal logic, candidate replay, and live E2E layout.
- 2026-08-21: Corrected the design boundary: DAO-spend raw transactions and
  ChallengeSpend are retained; snapshot voting is explicitly out of scope.
- 2026-08-21: Captured the V4 comparison baseline. For 100 votes: 59,119
  witness bytes, 59,876 transaction bytes, and 314,614,791 cycles. For 500
  votes: 295,249 witness bytes, 296,006 transaction bytes, and 1,566,465,014
  cycles. Baseline stripped ELF sizes: tally-type-script 226,040 bytes and
  vote-type-script 67,896 bytes.
- 2026-08-21: Implemented V5 compact VoteEvent proofs with direct CellDep
  indices and retained raw DAO-spend events. The builder test asserts compact
  votes carry no raw transaction while tracked spends do.
- 2026-08-21: Final V5 measurement at 100 votes: 43,325 witness bytes, 47,782
  transaction bytes, and 311,820,088 cycles. Compared with V4, this is 26.7%
  smaller witness, 20.2% smaller transaction, and 0.9% fewer cycles. The final
  stripped tally ELF is 229,904 bytes (+3,864, 1.7%); the Vote ELF is 71,304
  bytes (+3,408, 5.0%).
- 2026-08-21: The real local CKB E2E passed proposal creation, two votes, an
  intentionally omitted vote, successful challenge, complete re-tally,
  challenge-period maturity, final settlement, and Treasury payout. Report:
  `impl/target/live-e2e/1787274775-93275/report.json`.
