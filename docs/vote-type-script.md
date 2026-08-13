# Vote Type Script V1

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
dao_dep_indices: dao_dep_count * Uint16
```

On creation, the contract requires one live Open Proposal Cell in `cell_deps`, a
voter-owned input, and one vote output. DAO dep indices must be strictly
increasing, within the proposal's configured limit, owned by the vote output
lock, use the proposal-configured Nervos DAO code hash and hash type, and contain
eight zero data bytes (deposit phase). Their capacities must sum exactly to
`amount` and meet the proposal's minimum vote capacity. A referenced DAO cell
cannot also appear in the VoteTx inputs.

Consuming a Vote Cell is unrestricted. Settlement uses the historical VoteTx,
not the continued liveness of the small Vote Cell. The referenced DAO deposits
remain subject to the tally reducer's liveness rule.
