# VoteEvent Type Script V2

The canonical system design is in
[On-chain Voting with Optimistic Batch Settlement](./on-chain-voting-with-batch-settlement.md).

## Script

```text
args: Byte32 proposal_type_script_hash
```

## Data

```text
version:         byte = 1
direction:       byte, 0 = NO and 1 = YES
amount:          Uint64 little-endian shannon
dao_dep_count:   Uint16 little-endian
dao_out_points:  dao_dep_count * (Byte32 tx_hash + Uint32 output_index)
```

On creation, the contract requires one live Open Proposal Cell in `cell_deps`, a
live immutable Proposal Config Cell bound by that Proposal, a voter-owned input,
and one VoteEventCell output. DAO outpoints must be strictly increasing, within
the proposal's configured limit, present as direct CellDeps before any DepGroup,
owned by the vote output lock, use the Proposal Config's canonical Nervos DAO
code hash and hash type, and contain eight zero data bytes (deposit phase). The Proposal carries no duplicate
DAO or Vote identity. Its type script, the current Vote script, and each DAO dep
are checked directly against the same Proposal Config. DAO capacities must sum
exactly to `amount` and meet the proposal's minimum vote capacity. A referenced
DAO cell cannot also appear in the VoteTx inputs.

VoteEventCells are immutable and cannot be consumed, even with a valid lock
signature. This keeps their lock, type, and canonical vote data available as
CellDeps to every parallel tally and omitted-vote challenge. V2 deliberately has
no timeout reclaim path: a safe reclaim design needs durable on-chain evidence
that all tally and challenge work is over. The referenced DAO deposits remain
spendable. Spending one after VoteEventCell creation does not revoke the vote and
is not included in tally settlement.
