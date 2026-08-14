#![no_std]
#![no_main]

ckb_std::entry!(program_entry);
ckb_std::default_alloc!(16384, 1258306, 64);

use ckb_std::{
    ckb_constants::Source,
    ckb_types::prelude::Entity,
    high_level::{
        QueryIter, load_cell_data, load_cell_lock_hash, load_cell_type, load_cell_type_hash,
        load_script, load_script_hash,
    },
};
use treasury_common::{
    PolicyConfig, ProposalData, ResultData, TallyPhase, TallyState, blake2b_256,
};

#[repr(i8)]
enum Error {
    ArgsInvalid = 1,
    InvalidCellCount,
    ConfigNotFound,
    ConfigInvalid,
    ResultInvalid,
    ProposalNotFound,
    CandidateNotFound,
    ResultMismatch,
    InvalidPayout,
    ProposalPolicyMismatch,
}

pub fn program_entry() -> i8 {
    match run() {
        Ok(()) => 0,
        Err(error) => error as i8,
    }
}

fn run() -> Result<(), Error> {
    let script = load_script().map_err(|_| Error::ArgsInvalid)?;
    let config_type_hash: [u8; 32] = script
        .args()
        .raw_data()
        .as_ref()
        .try_into()
        .map_err(|_| Error::ArgsInvalid)?;
    let inputs = QueryIter::new(load_cell_type_hash, Source::GroupInput).count();
    let outputs = QueryIter::new(load_cell_type_hash, Source::GroupOutput).count();
    if inputs > 1 || outputs > 1 || inputs == outputs {
        return Err(Error::InvalidCellCount);
    }

    let (config, config_data) = load_config(config_type_hash)?;
    if load_script_hash()
        .map_err(|_| Error::ArgsInvalid)?
        .as_slice()
        != config.policy_type_hash
    {
        return Err(Error::ProposalPolicyMismatch);
    }
    if outputs == 1 {
        create_result(config_type_hash, config, &config_data)
    } else {
        consume_result(config)
    }
}

fn create_result(
    config_type_hash: [u8; 32],
    config: PolicyConfig,
    config_data: &[u8],
) -> Result<(), Error> {
    let result_data = load_cell_data(0, Source::GroupOutput).map_err(|_| Error::ResultInvalid)?;
    let result = ResultData::decode(&result_data).map_err(|_| Error::ResultInvalid)?;
    if result.policy_data_hash != blake2b_256(config_data) {
        return Err(Error::ResultMismatch);
    }

    let mut proposal = None;
    for (index, type_script) in QueryIter::new(load_cell_type, Source::Input).enumerate() {
        let Some(type_script) = type_script else {
            continue;
        };
        if type_script.calc_script_hash().as_slice() != result.proposal_id {
            continue;
        }
        let data = load_cell_data(index, Source::Input).map_err(|_| Error::ProposalNotFound)?;
        let parsed = ProposalData::decode(&data).map_err(|_| Error::ProposalNotFound)?;
        if proposal.replace((parsed, type_script)).is_some() {
            return Err(Error::ProposalNotFound);
        }
    }
    let (proposal, proposal_script) = proposal.ok_or(Error::ProposalNotFound)?;
    if proposal.policy_config_type_hash != config_type_hash
        || proposal.dao_code_hash != config.dao_code_hash
        || proposal.dao_hash_type != config.dao_hash_type
        || proposal_script.code_hash().as_slice() != config.proposal_code_hash
        || proposal_script.hash_type().as_slice()[0] != config.proposal_hash_type
        || proposal.vote_code_hash != config.vote_code_hash
        || proposal.vote_hash_type != config.vote_hash_type
        || proposal.tally_code_hash != config.tally_code_hash
        || proposal.tally_hash_type != config.tally_hash_type
        || proposal.policy_type_hash != config.policy_type_hash
    {
        return Err(Error::ProposalPolicyMismatch);
    }

    let mut candidate = None;
    for (index, type_script) in QueryIter::new(load_cell_type, Source::Input).enumerate() {
        if type_script.as_ref().is_some_and(|script| {
            script.code_hash().as_slice() == proposal.tally_code_hash
                && script.hash_type().as_slice()[0] == proposal.tally_hash_type
        }) {
            let data =
                load_cell_data(index, Source::Input).map_err(|_| Error::CandidateNotFound)?;
            let parsed = TallyState::decode(&data).map_err(|_| Error::CandidateNotFound)?;
            if parsed.phase == TallyPhase::Candidate
                && parsed.proposal_id == result.proposal_id
                && candidate.replace((parsed, data)).is_some()
            {
                return Err(Error::CandidateNotFound);
            }
        }
    }
    let (candidate, candidate_data) = candidate.ok_or(Error::CandidateNotFound)?;
    let expected_passed = config.passes(candidate.yes, candidate.no, proposal.requested_amount);
    if result.passed != expected_passed
        || result.requested_amount != proposal.requested_amount
        || result.receiver_lock_hash != proposal.receiver_lock_hash
        || result.yes != candidate.yes
        || result.no != candidate.no
        || result.final_state_hash != blake2b_256(&candidate_data)
    {
        return Err(Error::ResultMismatch);
    }
    Ok(())
}

fn consume_result(config: PolicyConfig) -> Result<(), Error> {
    let result_data = load_cell_data(0, Source::GroupInput).map_err(|_| Error::ResultInvalid)?;
    let result = ResultData::decode(&result_data).map_err(|_| Error::ResultInvalid)?;
    if !result.passed {
        return Ok(());
    }
    if !QueryIter::new(load_cell_lock_hash, Source::Input)
        .any(|lock_hash| lock_hash == config.treasury_lock_hash)
    {
        return Err(Error::InvalidPayout);
    }
    Ok(())
}

fn load_config(config_type_hash: [u8; 32]) -> Result<(PolicyConfig, alloc::vec::Vec<u8>), Error> {
    for (index, type_hash) in QueryIter::new(load_cell_type_hash, Source::CellDep).enumerate() {
        if type_hash == Some(config_type_hash) {
            let data = load_cell_data(index, Source::CellDep).map_err(|_| Error::ConfigInvalid)?;
            let config = PolicyConfig::decode(&data).map_err(|_| Error::ConfigInvalid)?;
            return Ok((config, data));
        }
    }
    Err(Error::ConfigNotFound)
}
