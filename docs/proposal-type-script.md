# Proposal and Settlement Scripts V1

The canonical system design is in
[On-chain Voting with Optimistic Batch Settlement](./on-chain-voting-with-batch-settlement.md).

The Proposal Type Script is an ordinary Rust CKB contract, not an embedded or
node-native script. It enforces Type ID uniqueness, `Open -> Closed` transition,
immutable proposal fields, proposal-bond capacity preservation, and one
`Closed + FinalCandidate -> Result` transition.

Creation also requires the immutable Policy Config Cell identified by
`policy_config_type_hash`. The Proposal's DAO code hash and hash type must equal
the canonical Nervos DAO identity in that configuration. The current Proposal
script and the Proposal's selected Vote, Tally, and Policy identities must also
match the versions authorized by that configuration. The Policy Type Script
checks the full binding again before it creates a Result Cell, so a proposal
that points at unrelated scripts or configuration cannot authorize a Treasury
payout.

TallySession Cells are independent Type-ID cells. Each batch verifies historical
transaction CBMT proofs, three SMT transitions, and the ordered reducer inside
CKB-VM. The final candidate has one challenge period. A valid omitted-vote or
omitted-DAO-spend proof consumes the candidate and transfers its bond to the
challenger. A mature candidate can create one Result Cell under the proposal's
versioned Policy Type Script.
