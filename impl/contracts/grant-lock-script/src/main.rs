#![no_std]
#![no_main]

ckb_std::entry!(program_entry);
ckb_std::default_alloc!(16384, 1258306, 64);

use ckb_std::{
    ckb_constants::Source,
    ckb_types::prelude::Entity,
    high_level::{QueryIter, load_cell_lock, load_cell_lock_hash, load_input_since, load_script},
    since::{LockValue, Since},
};

#[repr(i8)]
enum Error {
    ArgsInvalid = 1,
    PartialSpendForbidden,
    TimelockInvalid,
    BeneficiaryMissing,
}

pub fn program_entry() -> i8 {
    match run() {
        Ok(()) => 0,
        Err(error) => error as i8,
    }
}

fn run() -> Result<(), Error> {
    let script = load_script().map_err(|_| Error::ArgsInvalid)?;
    let args = script.args().raw_data();
    if args.len() != 40 {
        return Err(Error::ArgsInvalid);
    }
    let unlock_block = u64::from_le_bytes(args[..8].try_into().unwrap());
    let beneficiary_lock_hash: [u8; 32] = args[8..].try_into().unwrap();

    if QueryIter::new(load_cell_lock, Source::Output)
        .any(|output_lock| output_lock.as_slice() == script.as_slice())
    {
        return Err(Error::PartialSpendForbidden);
    }
    if QueryIter::new(load_input_since, Source::GroupInput).any(|raw_since| {
        let since = Since::new(raw_since);
        !since.flags_is_valid()
            || !since.is_absolute()
            || !matches!(
                since.extract_lock_value(),
                Some(LockValue::BlockNumber(block)) if block >= unlock_block
            )
    }) {
        return Err(Error::TimelockInvalid);
    }
    if !QueryIter::new(load_cell_lock_hash, Source::Input)
        .any(|lock_hash| lock_hash == beneficiary_lock_hash)
    {
        return Err(Error::BeneficiaryMissing);
    }
    Ok(())
}
