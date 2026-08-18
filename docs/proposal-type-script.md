# Proposal and Settlement Scripts V1

The canonical system design is in
[On-chain Voting with Optimistic Batch Settlement](./on-chain-voting-with-batch-settlement.md).

The Proposal Type Script is an ordinary Rust CKB contract, not an embedded or
node-native script. It enforces Type ID uniqueness, `Open -> Closed` transition,
immutable proposal fields, proposal-bond capacity preservation, and one
`Closed + FinalCandidate -> Result` transition.

Creation also requires the immutable Proposal Config Cell identified by
`proposal_config_type_hash`. Proposal data contains no DAO, Vote, Tally, or
Policy identity of its own. The current Proposal script must match the version
authorized by Proposal Config; all later Vote, Tally, Result, and Treasury paths
resolve their protocol identities from that same immutable cell. Proposal
Config is therefore the single protocol-identity source rather than a second
copy checked against proposer-supplied fields.

Proposal data contains proposal-specific limits, amount, receiver, voting and
challenge windows, the Proposal Config reference, and a metadata commitment.
Only the lifecycle phase may change when an Open Proposal becomes Closed.

TallySession Cells are independent Type-ID cells. Each batch verifies historical
transaction CBMT proofs, one unified SMT transition, and the ordered reducer
inside CKB-VM. Every TallySession transaction includes the referenced Proposal
Config Cell and verifies both the Proposal and Tally code identities against it.
The final candidate has one challenge period. A valid omitted-vote or
omitted-DAO-spend proof consumes the candidate and transfers its bond to the
affected voter proven by that omission. A mature candidate can create one Result Cell under the Policy Type
Script authorized by Proposal Config.
