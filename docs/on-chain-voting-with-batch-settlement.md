# On-chain Voting with Optimistic Batch Settlement

## Status

This document describes the V5 tally-witness implementation in `impl/`. VoteEventCells,
batch verification, challenges, passing-policy evaluation, treasury payout, burn,
and grant timelocks execute in CKB-VM. The CKB node does not maintain a tally
index and does not scan historical voting windows during transaction validation.

## Architecture

```mermaid
flowchart LR
    P["Open Proposal Cell"] --> V["Immutable VoteEventCells on chain"]
    V --> C["Closed Proposal Cell"]
    C --> S["Operator creates TallySession + bond"]
    S --> B1["Batch 1: block CBMT multiproofs + SMT transition"]
    B1 --> BN["Batch N: consume prior session"]
    BN --> F["FinalCandidate"]
    F -->|"valid omission proof"| X["Candidate removed; bond to challenger"]
    F -->|"challenge period expires"| R["Passed or Failed Result Cell"]
    R -->|"passed"| T["Treasury payout"]
    R -->|"failed"| Z["No treasury access"]
```

The result transaction and treasury payout are separate. This keeps vote
settlement independent from Treasury Cell selection and allows a passed result to
be consumed exactly once by the payout transaction.

## Cells and scripts

- **Proposal Cell**: a Type-ID singleton. It moves from `Open` to `Closed`, then
  is consumed with one mature FinalCandidate to create one Result Cell. It
  stores proposal-specific parameters, one `proposal_config_type_hash`, and a
  metadata commitment; it does not duplicate protocol script or DAO identities.
- **VoteEventCell**: records `YES` or `NO`, the claimed amount, and canonical DAO
  outpoints. Its outpoint authenticates the VoteTx hash used by the transaction
  position proof. Its type args contain the full Proposal type-script hash. The
  Proposal binds an immutable Proposal Config whose canonical DAO code hash and
  hash type are checked again by the Vote and Policy scripts. This keeps network
  identity configurable without allowing a Proposal to self-authorize a fake
  DAO-like script. A DAO outpoint cannot simultaneously be a VoteTx `cell_dep`
  and an input; both the Vote Script and tally reducer reject this. The cell is
  immutable so a voter cannot destroy evidence before tally or challenge. V5
  intentionally leaves reclaim as unresolved lifecycle work.
- **TallySession Cell**: a Type-ID singleton owned logically by one operator. Its
  capacity is the settlement bond, which must be at least Proposal Config's
  absolute minimum and does not scale with the Proposal's requested payout.
  Active sessions use the operator lock. Each
  batch consumes the previous session and creates the next state. The final
  Candidate must switch to the permissionless lock hash committed by Proposal
  Config, so a valid challenge never needs the operator's signature. Creation,
  advance, challenge, and finalization all include the Proposal Config Cell and
  resolve authorized Proposal, Vote, Tally, and Candidate-lock identities from it.
- **Result Cell**: evaluated by a versioned Policy Type Script. A passed result may
  only be consumed by the configured Treasury Lock's payout action; the burn
  action explicitly rejects Result inputs.
- **Treasury Cell**: created by CKB consensus using one fixed Treasury Lock. It can
  be spent by a passed result or burned after expiry.
- **Grant Cell**: optional payout lock with an absolute block timelock and a
  beneficiary lock hash.
- **Proposal Config Cell**: an immutable Type-ID cell and the single source for
  the canonical Nervos DAO identity, authorized Proposal, Vote, and Tally code
  identities, exact Policy Type Script hash, passing rules, global proposal
  amount cap, minimum challenge period, and minimum tally bond. An upgrade creates
  a new Proposal Config Cell; existing proposals continue to reference their
  original configuration. Test and devnet configurations use five blocks and a
  5,000 CKB absolute bond floor. Production deployments should use the same
  5,000 CKB bond floor initially and at least 8,640 blocks (about 24 hours at a
  ten-second block interval), unless a stronger network-specific analysis selects
  a larger bond or longer window.

## VoteRecord

`VoteRecord` is the preimage committed as the value of a voter leaf in the vote
SMT. It is not merely a cached vote count.

```text
VoteRecord {
    voter_lock_hash: Byte32,
    direction:       0 | 1,
    amount:          Uint64,
    block_number:    Uint64,
    tx_index:        Uint32,
    dao_out_points:  Vec<OutPoint>,
}
```

The unified state SMT uses separate vote, DAO, and event key namespaces. Each
physical key is `blake2b("CKB Treasury state key V1" || namespace || logical_key)`,
so the namespaces retain the full hash output instead of reserving or truncating
key bits. The vote namespace stores `voter_lock_hash -> blake2b(VoteRecord)`,
while the DAO namespace stores `dao_out_point_key -> voter_lock_hash`. When a
voter votes again, the batch witness reveals the old VoteRecord, proves that its
hash is the current vote-leaf value, removes all old DAO mappings and old weight,
then installs the new record. When a referenced DAO deposit is spent, the DAO
leaf identifies the voter and the same VoteRecord provides the complete list of
mappings and weight to remove.

This preimage is required because an SMT value hash alone cannot tell the
contract which DAO outpoints must be deleted during revote or invalidation.

## Tally state

Each TallySession commits to one domain-separated SMT with three logical namespaces:

- vote: voter lock hash to VoteRecord hash;
- DAO: DAO outpoint key to voter lock hash;
- event: historical transaction hash to `EVENT_PRESENT`.

The current fixed-length `TallyState` layout retains the `votes_root`, `dao_root`,
and `events_root` fields, but V5 requires all three fields to contain the same
unified state root. A transition or candidate with unequal roots is invalid. This
keeps the state layout stable while reducing each non-empty batch to one compiled
SMT proof verified against both the old and new roots.

It also stores the next scan cursor, sequence number, `yes`, `no`, processed event
count, operator lock hash, and the final voting-window anchor. Finalization time is
not derived from that historical anchor: it uses the Candidate input Cell's
relative block-number `since`, so the challenge clock starts when the Candidate
is actually created on chain.

```mermaid
stateDiagram-v2
    [*] --> Active: create session and lock bond
    Active --> Active: verified batch and partial cursor
    Active --> Candidate: verified final batch reaches end cursor
    Candidate --> [*]: valid omission challenge, slash bond
    Candidate --> [*]: challenge period expires, create Result
```

Only a transaction containing an input with the operator lock hash may advance or
finalize the session. Finalization additionally requires a relative block-number
`since` at least equal to the proposal challenge period on the Candidate input.
Active batches preserve their lock, while the final batch must use Proposal
Config's canonical permissionless Candidate lock. A challenge is therefore
permissionless at both the lock and type-script layers. Competing operators may
create independent sessions, but only one can consume the singleton Closed
Proposal during finalization.

## Batch witness verification

A batch contains only relevant historical events: compact VoteEvent proofs and
raw transactions that spend a currently tracked DAO outpoint. Events are grouped by block. Each
block group carries:

- block number, `header_dep` index, total transaction count, and witnesses root;
- strictly increasing transaction indices; each vote carries a VoteEventCell
  outpoint, direct CellDep index, voter lock hash, and canonical VoteData;
- a serialized RawTransaction only when the event spends a tracked DAO outpoint;
- one CBMT multiproof shared by all included transactions from that block.

The contract performs the following checks:

1. Load each referenced header once and match its block number.
2. Derive a vote transaction hash from its immutable VoteEventCell outpoint, or
   hash the raw DAO-spend transaction. For a mixed vote/spend event, require both
   hashes to match. Verify the block-level CBMT multiproof with each hash bound
   to its strictly ordered transaction index.
3. Combine the computed raw-transaction root and supplied witnesses root, then
   require the result to equal the header's `transactions_root`.
4. Verify one compiled SMT proof against both the old and new unified roots for
   every touched, domain-separated key.
5. Re-run the reducer in `(block_number, tx_index)` order.
6. Load every referenced VoteEventCell from a direct CellDep and require its
   configured Vote Type, proposal args, lock hash, and data to match the proof.
7. Require the recomputed leaf values, totals, event count, cursor, roots, and next
   phase to equal the output TallySession.

```mermaid
flowchart TD
    W["Offline builder emits batch witness"] --> H["Load header_dep"]
    H --> M["Verify block CBMT multiproof"]
    M --> O["Verify old unified SMT root"]
    O --> D["Apply ordered revote and DAO-spend reducer"]
    D --> N["Verify new unified SMT root and yes/no totals"]
    N --> Q["Create next TallySession"]
```

The proposal fixes `max_events_per_batch`, `max_dao_deps_per_vote`,
`max_state_keys_per_batch`, `max_batch_witness_bytes`, and
`max_batch_sequence`. These are consensus-enforced limits, not SDK hints. The raw
Tally witness byte length is checked before decoding variable-length proof and
event vectors, so an oversized witness cannot force allocations before the
configured limit is applied.

VoteEvent and DAO dependencies must precede any DepGroup in a tally transaction,
so the proof's raw CellDep index is also the resolved `Source::CellDep` index.
Standard lock DepGroups may follow this direct-dependency prefix.

An empty batch is valid only when it preserves the unified root in all three
layout fields, both tally totals, and carries no transitions, prior records, or
SMT proofs. This lets a
no-vote proposal, or an empty suffix of a voting window, reach `FinalCandidate`
without granting the operator any ability to alter state.

## Off-chain operator path

The Rust `tally-builder` includes a blocking CKB RPC adapter. It requests each
canonical block in serialized Molecule form, recomputes the raw-transaction and
witness CBMT roots, checks them against the header, and scans transactions in
chain order. The builder receives the same decoded Proposal Config used by the
contracts and recognizes Vote scripts only through its authorized identity.
Temporary vote and DAO state is carried across blocks, so a DAO
deposit spent in a later block removes a vote found earlier in the same batch.
Consecutive RPC blocks must also form one parent-hash chain; a reorg during a
scan causes an immediate retry instead of producing a mixed-branch batch.

The scanner returns the proven events, next cursor, candidate anchor, and the
ordered header hashes that the transaction assembler must use as `header_deps`.
`CkbRpcClient` also submits a signed, assembled CKB transaction with the
`passthrough` output validator. Wallet selection, fee inputs, signing, and the
final transaction layout remain caller responsibilities.

## Why the final candidate is optimistic

CBMT proofs prove that every submitted event exists, but they cannot prove that
the operator submitted every relevant event. Intermediate batches therefore do
not wait for a challenge period. The complete event namespace is challenged only
after the final cursor is reached.

Two V5 challenges are supported:

- **Omitted vote**: load the immutable VoteEventCell, prove its transaction hash
  is included at the claimed block position, and prove that hash is absent from
  the event namespace. The raw VoteTx is not included.
- **Omitted DAO spend**: prove that the final DAO namespace still maps an outpoint
  to a nonzero voter, prove a transaction in the voting window spends that
  outpoint, and prove the spend transaction is absent from the event namespace. This
  final-state condition prevents an obsolete outpoint from an earlier,
  superseded vote from producing a false challenge.

The compact path removes the malicious oversized-VoteTx witness problem because
unrelated VoteTx inputs, outputs, and cell deps are not copied into settlement.
DAO-spend raw transactions remain necessary for deposit-liveness checks. A
block-sized DAO-spend transaction is therefore a separate residual sizing risk,
not something V5 claims to eliminate.

A successful challenge consumes the candidate and pays its entire bond to the
challenge transaction sender. CKB transactions have no native sender field, so
the contract defines the sender as the lock hash of the first input other than
the Candidate Cell. The challenge must include this authorization/fee input and
one plain output with exactly the Candidate bond under the same lock. The sender
lock hash is derived from transaction inputs and is not carried in the witness.

This one-transaction flow does not provide consensus-level front-running
protection: a mempool observer can copy the public omission proof, rebuild the
transaction with an input they control, and compete to consume the same
Candidate Cell. Deployments accept this trade-off unless they add a private
relay or a separate commit-reveal protocol. A successful challenge does not
rewrite history; another operator can start a fresh session from the Closed
Proposal.

## Passing policy

The Result Type Script loads an immutable Proposal Config Cell and evaluates:

```text
total = yes + no
passed = requested_amount <= maximum_proposal_amount
      && total >= minimum_total_votes
      && yes * 10_000 >= total * approval_bps
```

The Proposal Config data hash is committed in the Result Cell. The Policy script
requires the Proposal's `proposal_config_type_hash` to identify the same config,
then checks the Proposal input, Tally candidate, current Policy script, passing
rule, and Treasury identity directly against that one source. Changing policy
means deploying a new immutable Proposal Config version and creating future
proposals that reference it. Node Rust code is unaffected.

## Treasury payout and burn

The node keeps `secondary_epoch_reward` unchanged and materializes only future
would-be-burned issuance after activation. Historical burned CKB is never
recreated. Multiple target blocks are aggregated before one Treasury Cell is
emitted.

For payout, the Treasury Lock requires exactly one passed Result input. If the
Treasury inputs sum to `T` and the proposal requests `A`, the transaction must
create:

- exactly one receiver output with capacity `A`, no Type Script, and empty data;
  this output must be distinct from Treasury change; and
- zero or one Treasury change output with capacity `T - A`.

Treasury capacity cannot pay transaction fees; external inputs fund fees.

For burn, every Treasury input must use a relative block-number `since` at least
`burn_expiry_blocks`. The transaction creates one configured zero-lock output
and one caller incentive output. Their capacities must sum exactly to the
Treasury inputs. The incentive is:

```text
min(
  base_burn_incentive
    + (relative_blocks - burn_expiry_blocks) * burn_incentive_rate,
  maximum_burn_incentive
)
```

The fixed Treasury lock args contain the immutable Treasury Config type hash.
Treasury creation height is not encoded in args; relative `since` uses each
input Cell's actual creation point and naturally resets for Treasury change.

## Implemented verification

- Rust unit tests cover canonical codecs, VoteRecord commitments, policy math,
  single and block-level CBMT proofs, state-key namespaces, and old/new SMT
  transition proofs.
- `ckb-testtool` rejects duplicate or unsorted transaction indices, added or
  removed CBMT lemmas, wrong block headers, and modified RawTransactions.
- `ckb-testtool` executes a builder-generated final batch in CKB-VM.
- `ckb-testtool` executes an omitted-vote challenge and bond slash.
- `ckb-testtool` executes exact Treasury payout and expired burn paths.
- CKB node tests cover activation, issuance decomposition, derived-state replay,
  Cellbase creation, full block reward verification, and canonical reorg behavior.
- The reproducible `live-e2e` runner starts the current Treasury-enabled CKB
  binary and submits real transactions through RPC, tx-pool, proposal, block
  assembly, and block verification. It mines DAO deposits, an open/closed
  proposal, and VoteTxs; accepts and then slashes an omitted-vote candidate;
  accepts the complete candidate; finalizes a passed Result Cell; and consumes a
  consensus-created Treasury Cell for payout.
- In the validated run, the incomplete candidate and challenge committed in
  blocks 56 and 60. The complete candidate, finalization, and payout committed in
  blocks 68, 72, and 76. The tally was 2,100 CKB YES and 0 NO; the payout sent
  100 CKB and preserved the exact Treasury change.
- The RPC adapter accepts both current 5-field Molecule `BlockV1` responses and
  legacy 4-field `Block` responses, and verifies their transaction roots before
  building proofs.

Remaining production work includes deployment manifests, production
wallet/signing and transaction-assembly tooling, complex-event benchmarks,
larger multi-batch soak tests, and final parameter selection.
