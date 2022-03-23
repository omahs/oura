use std::ops::Deref;

use log::info;

use pallas::network::{
    miniprotocols::{handshake, run_agent, MAINNET_MAGIC},
    multiplexer::Channel,
};

use serde::Deserialize;

use crate::{
    mapper::{Config as MapperConfig, EventWriter},
    pipelining::{new_inter_stage_channel, PartialBootstrapResult, SourceProvider},
    sources::{
        common::{AddressArg, MagicArg, PointArg},
        define_start_point, setup_multiplexer, IntersectArg, RetryPolicy,
    },
    utils::{ChainWellKnownInfo, WithUtils},
    Error,
};

use super::run::observe_forever;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub address: AddressArg,

    #[serde(deserialize_with = "crate::sources::common::deserialize_magic_arg")]
    pub magic: Option<MagicArg>,

    #[deprecated(note = "use intersect value instead")]
    pub since: Option<PointArg>,

    pub intersect: Option<IntersectArg>,

    #[deprecated(note = "chain info is now pipeline-wide, use utils")]
    pub well_known: Option<ChainWellKnownInfo>,

    #[serde(default)]
    pub mapper: MapperConfig,

    /// Min block depth (# confirmations) required
    ///
    /// The min depth a block requires to be considered safe to send down the
    /// pipeline. This value is used to configure a rollback buffer used
    /// internally by the stage. A high value (eg: ~6) will reduce the
    /// probability of seeing rollbacks events. The trade-off is that the stage
    /// will need some time to fill up the buffer before sending the 1st event.
    #[serde(default)]
    pub min_depth: usize,

    pub retry_policy: Option<RetryPolicy>,
}

fn do_handshake(channel: &mut Channel, magic: u64) -> Result<(), Error> {
    let versions = handshake::n2c::VersionTable::v1_and_above(magic);
    let agent = run_agent(handshake::Initiator::initial(versions), channel)?;
    info!("handshake output: {:?}", agent.output);

    match agent.output {
        handshake::Output::Accepted(_, _) => Ok(()),
        _ => Err("couldn't agree on handshake version for client connection".into()),
    }
}

impl SourceProvider for WithUtils<Config> {
    fn bootstrap(&self) -> PartialBootstrapResult {
        let (output_tx, output_rx) = new_inter_stage_channel(None);

        let mut muxer = setup_multiplexer(
            &self.inner.address.0,
            &self.inner.address.1,
            &[0, 5],
            &self.inner.retry_policy,
        )?;

        let magic = match &self.inner.magic {
            Some(m) => *m.deref(),
            None => MAINNET_MAGIC,
        };

        let writer = EventWriter::new(output_tx, self.utils.clone(), self.inner.mapper.clone());

        let mut hs_channel = muxer.use_channel(0);
        do_handshake(&mut hs_channel, magic)?;

        let mut cs_channel = muxer.use_channel(5);

        let known_points = define_start_point(
            &self.inner.intersect,
            #[allow(deprecated)]
            &self.inner.since,
            &self.utils,
            &mut cs_channel,
        )?;

        info!("starting chain sync from: {:?}", &known_points);

        let min_depth = self.inner.min_depth;
        let handle = std::thread::spawn(move || {
            observe_forever(cs_channel, writer, known_points, min_depth)
                .expect("chainsync loop failed");
        });

        Ok((handle, output_rx))
    }
}
