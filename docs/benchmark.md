# Treasury Voting Benchmarks

## Optimistic batch settlement

The implemented benchmark is the ignored test
`contract_tests::tests::benchmark_tally_batch_cycles`. It creates independent
voters and one DAO outpoint per voter. The builder-produced transaction then runs
in CKB-VM through `ckb-testtool`.

Run it with:

```bash
cd impl
make build
cargo test -p contract-tests benchmark_tally_batch_cycles -- --ignored --nocapture
```

Measured on Apple M2 Pro (`arm64`) with Rust 1.95.0 and the contract build's
release profile plus debug assertions.

Sizes in this document use decimal KB (`1 KB = 1,000 bytes`).

### Reproduced V1 baseline

| Independent votes | CKB-VM cycles | Cycles per vote | Batch witness | Settlement tx |
|---:|---:|---:|---:|---:|
| 1 | 3,250,782 | 3,250,782 | 0.670 KB | 1.390 KB |
| 10 | 32,333,827 | 3,233,382 | 7.509 KB | 8.229 KB |
| 50 | 162,938,523 | 3,258,770 | 41.093 KB | 41.813 KB |
| 100 | 327,064,874 | 3,270,648 | 85.387 KB | 86.107 KB |

### Optimized V3

V3 preserves the same header commitment, RawTransaction hashing, reducer, old/new
state-root verification, and final omission challenges. It changes only the
proof representation and deterministic implementation:

- transition keys must be strictly increasing, replacing quadratic duplicate
  detection with one linear pass;
- all relevant transactions from one block share one CBMT multiproof and one
  copy of block metadata;
- vote, DAO-outpoint, and processed-event leaves use BLAKE2b domain-separated
  namespaces in one SMT, verified once against both the old and new roots;
- the tally witness has an explicit V4 version, so older encodings are rejected.

| Independent votes | CKB-VM cycles | Cycles per vote | Batch witness | Settlement tx |
|---:|---:|---:|---:|---:|
| 1 | 3,292,919 | 3,292,919 | 0.662 KB | 1.382 KB |
| 10 | 31,740,341 | 3,174,034 | 5.977 KB | 6.697 KB |
| 20 | 63,291,558 | 3,164,577 | 11.885 KB | 12.605 KB |
| 21 | 66,409,707 | 3,162,367 | 12.468 KB | 13.188 KB |
| 22 | 69,548,994 | 3,161,317 | 13.065 KB | 13.785 KB |
| 23 | 72,695,867 | 3,160,689 | 13.654 KB | 14.374 KB |
| 50 | 157,696,837 | 3,153,936 | 29.605 KB | 30.325 KB |
| 100 | 314,427,610 | 3,144,276 | 59.099 KB | 59.819 KB |
| 200 | 628,181,614 | 3,140,908 | 118.189 KB | 118.909 KB |
| 500 | 1,566,878,583 | 3,133,757 | 295.281 KB | 296.001 KB |

At 100 independent votes, V3 reduces cycles by 3.9% and witness bytes by 30.8%
relative to the reproduced V1 baseline. The verification cost remains linear at
about 3.13M to 3.17M cycles per independent vote.

The default 70M `tx_pool.max_tx_verify_cycles` is not a consensus or current
transaction-admission ceiling. It is primarily the local threshold that marks a
remote transaction as large-cycle for verification-worker scheduling. Therefore,
22 votes at 69.55M and 23 votes at 72.70M fall on different sides of that
scheduling threshold, but both remain valid transaction-pool and relay candidates.

Consensus block verification gives each transaction up to the consensus
`max_block_cycles` and also requires the sum for the block to stay within that
same limit. A 100-vote batch uses about 9.0% of CKB's default 3.5B block cycle
limit and 10.0% of the default 597 KB block limit, so it fits both measured
consensus resources. Raising `tx_pool.max_tx_verify_cycles` is not required to
accept it; doing so mainly lets more workers handle it as a small-cycle
transaction and reduces the intended isolation of expensive remote transactions.

The implemented 100-event cap is therefore a reasonable current builder
target for the independent-vote shape, rather than a 20-event ceiling. These
figures are still preliminary: revote-heavy, multi-DAO-deposit, sparse
cross-block, and adversarial proof shapes need separate measurements before the
production cap is frozen.

### V5 VoteEventCell position proofs

V5 replaces each raw VoteTx in the tally witness with the immutable
VoteEventCell outpoint, a direct CellDep index, voter lock hash, and canonical
VoteData. Raw DAO-spend transactions and `ChallengeSpend` are unchanged. The
same checkout immediately before the V5 change measured a 59.119 KB witness,
a 59.876 KB transaction, and 314,614,791 cycles for 100 votes.

| Independent votes | CKB-VM cycles | Cycles per vote | Batch witness | Settlement tx |
|---:|---:|---:|---:|---:|
| 1 | 3,331,596 | 3,331,596 | 0.506 KB | 1.300 KB |
| 10 | 31,532,187 | 3,153,218 | 4.403 KB | 5.530 KB |
| 20 | 62,672,356 | 3,133,617 | 8.715 KB | 10.212 KB |
| 50 | 156,132,217 | 3,122,644 | 21.693 KB | 24.300 KB |
| 100 | 311,820,088 | 3,118,200 | 43.325 KB | 47.782 KB |
| 200 | 623,495,528 | 3,117,477 | 86.573 KB | 94.730 KB |
| 500 | 1,555,171,235 | 3,110,342 | 216.267 KB | 235.524 KB |

At 100 votes, V5 reduces witness bytes by 26.7% and complete settlement
transaction bytes by 20.2% relative to that V4 baseline. Cycles decrease by
0.9%. Each vote adds one 0.037 KB CellDep to the settlement transaction, which
is why total transaction size shrinks less than witness size. The one-DAO
VoteEventCell data grows from 0.014 KB to 0.048 KB because it commits the actual DAO
outpoint instead of a VoteTx-local CellDep index.

The stripped tally contract grows from 226.040 KB to 229.904 KB (+3.864 KB, 1.7%),
and the Vote contract grows from 67.896 KB to 71.304 KB (+3.408 KB, 5.0%). Both
remain below the 409.6 KB warning threshold. V5 does not reduce the size of a raw
DAO-spend event; that path needs a separate worst-case benchmark and mitigation.

### V6 vote-time DAO eligibility

V6 validates that every referenced DAO Cell is live and old enough when the
VoteEventCell is created. A later DAO spend does not revoke the vote, so tally no
longer scans or embeds DAO-spend transactions. The DAO-outpoint SMT namespace,
`ChallengeSpend`, and DAO outpoints in `VoteRecord` are removed. `TallyState`
stores one `state_root`; each independent vote updates one vote leaf and one
event leaf.

| Independent votes | CKB-VM cycles | Cycles per vote | Batch witness | Settlement tx | SMT proof |
|---:|---:|---:|---:|---:|---:|
| 1 | 2,283,194 | 2,283,194 | 0.402 KB | 1.132 KB | 0.009 KB |
| 10 | 21,140,840 | 2,114,084 | 3.349 KB | 4.412 KB | 0.085 KB |
| 50 | 104,579,905 | 2,091,598 | 16.473 KB | 19.016 KB | 0.449 KB |
| 100 | 208,647,822 | 2,086,478 | 32.869 KB | 37.262 KB | 0.895 KB |
| 200 | 417,803,129 | 2,089,015 | 65.675 KB | 73.768 KB | 1.801 KB |
| 500 | 1,042,713,890 | 2,085,427 | 164.097 KB | 183.290 KB | 4.523 KB |
| 1,000 | 2,080,380,817 | 2,080,380 | 328.035 KB | 365.728 KB | 8.961 KB |
| 1,365 | 2,839,164,998 | 2,079,974 | 447.750 KB | 498.948 KB | 12.241 KB |

For the benchmark's ideal initial-state shape (one block, one DAO deposit and one
direct VoteEvent CellDep per independent voter), 1,365 votes verify under the
default 3.5B cycle and 597 KB block limits. At 1,366 votes the Tally Type
Script fails a deterministic heap allocation before either consensus resource
limit is reached. This is an implementation boundary, not a recommended protocol
cap. Multi-block proofs, revotes, mature SMT state, and multiple DAO deposits per
vote require margin.

The ignored `benchmark_compiled_smt_proof_sizes` test also measures a 100-vote
batch as 200 absent SMT keys against progressively larger synthetic committed
state:

| Existing nonzero leaves | Target leaves | Compiled SMT proof |
|---:|---:|---:|
| 0 | 200 | 0.889 KB |
| 300 | 200 | 13.925 KB |
| 3,000 | 200 | 34.875 KB |
| 30,000 | 200 | 56.983 KB |

The 0.889 KB synthetic empty-tree result differs slightly from the complete
100-vote transaction's 0.895 KB proof because the actual domain-separated keys
have a different branching shape. Proof size grows with accumulated state and
key distribution, so production sizing must not extrapolate only from an empty
tree.

## Legacy node-scan proposal benchmark

Unlike a normal script on CKB, the proposal type script needs to perform calculations over a large number of blocks, which could become a bottleneck. Hence we need to design a benchmark and measure it.

## Probe
We add USDT (User Statically-Defined Tracing) to the proposal type script for this task. We define the following probes:

```rust
#[usdt::provider]
pub mod proposal_probe {
    fn verify_entry() {}
    fn verify_exit() {}
    fn block_provider_entry() {}
    fn block_provider_exit() {}
}
```

They measure the `verify` function and block loading. The former gives an overview, while block loading reveals where the bottleneck lies.

## Steps to Bench
1. Build ckb with the `probe` feature enabled (it is enabled by default).
2. Run `e2e/start.sh` to start ckb.
3. Run `e2e/benchmark.sh` to start the benchmark.
4. Run `e2e/run-devnet.sh` to start the test case. Use `export DURATION=N` to specify the duration to measure.
5. Press `Ctrl+C` on `benchmark.sh` when testing is done.

## Results
The following scenarios were used:
1. 1000 blocks
2. Each block contains 1051 transactions
3. Machine: Intel(R) Xeon(R) Platinum 8275CL CPU @ 3.00GHz, 4 cores.

Results are as follows:
```text
[verify ] TID 222364  call# 1     6114         ns
[verify ] TID 222366  call# 1     2087422275   ns
^C
╔════════════════════════════════════════════════════╗
║                verify()  Summary                   ║
╚════════════════════════════════════════════════════╝
@verify_count: 2
@verify_total_ns: 6382395685
@verify_avg_ns: 3191197842

@verify_min_ns: 6114
@verify_max_ns: 6382389571

  Latency distribution (ns):
@verify_quant_ns:
[4K, 8K)               1 |@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@|
[8K, 16K)              0 |                                                    |
[16K, 32K)             0 |                                                    |
[32K, 64K)             0 |                                                    |
[64K, 128K)            0 |                                                    |
[128K, 256K)           0 |                                                    |
[256K, 512K)           0 |                                                    |
[512K, 1M)             0 |                                                    |
[1M, 2M)               0 |                                                    |
[2M, 4M)               0 |                                                    |
[4M, 8M)               0 |                                                    |
[8M, 16M)              0 |                                                    |
[16M, 32M)             0 |                                                    |
[32M, 64M)             0 |                                                    |
[64M, 128M)            0 |                                                    |
[128M, 256M)           0 |                                                    |
[256M, 512M)           0 |                                                    |
[512M, 1G)             0 |                                                    |
[1G, 2G)               0 |                                                    |
[2G, 4G)               0 |                                                    |
[4G, 8G)               1 |@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@|


╔════════════════════════════════════════════════════╗
║          BlockProvider calls  Summary              ║
╚════════════════════════════════════════════════════╝
@bp_count: 1002
@bp_total_ns: 5837837932
@bp_avg_ns: 5826185

@bp_min_ns: 2491
@bp_max_ns: 17768858

  Latency distribution (ns):
@bp_quant_ns:
[2K, 4K)               1 |                                                    |
[4K, 8K)               1 |                                                    |
[8K, 16K)              0 |                                                    |
[16K, 32K)             0 |                                                    |
[32K, 64K)             0 |                                                    |
[64K, 128K)            0 |                                                    |
[128K, 256K)           0 |                                                    |
[256K, 512K)           0 |                                                    |
[512K, 1M)             0 |                                                    |
[1M, 2M)               0 |                                                    |
[2M, 4M)             477 |@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@|
[4M, 8M)             149 |@@@@@@@@@@@@@@@@                                    |
[8M, 16M)            373 |@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@            |
[16M, 32M)             1 |                                                    |
```

It costs 2.1 seconds in total. Normalized to 1 day, that is 22.7 seconds (158.8 seconds for 7 days).
Although this scenario is at maximum throughput, the processing time is significant. We need a plan to reduce the total workload.
