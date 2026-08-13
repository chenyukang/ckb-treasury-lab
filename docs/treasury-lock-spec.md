# Treasury Lock Script V1

The canonical system design is in
[On-chain Voting with Optimistic Batch Settlement](./on-chain-voting-with-batch-settlement.md).

## Script

```text
args: Byte32 immutable_treasury_config_type_hash
```

The node emits a fixed lock script. Creation block numbers are not placed in
args; burn maturity uses relative block-number `since` values on Treasury
inputs.

## Witness actions

```text
0x00 || caller_lock_hash  burn expired Treasury inputs
0x01                     pay one passed Result
```

Payout conserves all Treasury input capacity between the exact receiver amount
and optional Treasury change. Burn conserves it between the configured zero-lock
output and capped caller incentive. External inputs pay transaction fees in both
paths.
