# Proposal and Settlement Scripts V1

The canonical system design is in
[On-chain Voting with Optimistic Batch Settlement](./on-chain-voting-with-batch-settlement.md).

The Proposal Type Script is an ordinary Rust CKB contract, not an embedded or
node-native script. It enforces Type ID uniqueness, `Open -> Closed` transition,
immutable proposal fields, proposal-bond capacity preservation, and one
`Closed + FinalCandidate -> Result` transition.

TallySession Cells are independent Type-ID cells. Each batch verifies historical
transaction CBMT proofs, three SMT transitions, and the ordered reducer inside
CKB-VM. The final candidate has one challenge period. A valid omitted-vote or
omitted-DAO-spend proof consumes the candidate and transfers its bond to the
challenger. A mature candidate can create one Result Cell under the proposal's
versioned Policy Type Script.
