use core::mem;
use core::time::Duration;

use serde::{Deserialize, Serialize};

use embedded_svc::channel::asyncs::Receiver;
use embedded_svc::channel::asyncs::Sender;
use embedded_svc::mutex::{Mutex, MutexFamily};
use embedded_svc::signal::asyncs::{SendSyncSignalFamily, Signal};
use embedded_svc::sys_time::SystemTime;
use embedded_svc::timer::asyncs::OnceTimer;
use embedded_svc::utils::asyncs::select::select;
use embedded_svc::utils::asyncs::select::Either;

use crate::error;
use crate::state_snapshot::StateSnapshot;
use crate::storage::*;
use crate::utils::as_static_receiver;
use crate::water_meter::WaterMeterState;

const FLOW_STATS_INSTANCES: usize = 8;

const DURATIONS: [Duration; FLOW_STATS_INSTANCES] = [
    Duration::from_secs(60 * 5),
    Duration::from_secs(60 * 30),
    Duration::from_secs(60 * 60),
    Duration::from_secs(60 * 60 * 6),
    Duration::from_secs(60 * 60 * 12),
    Duration::from_secs(60 * 60 * 24),
    Duration::from_secs(60 * 60 * 24 * 7),
    Duration::from_secs(60 * 60 * 24 * 30),
];

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FlowSnapshot {
    time: Duration,
    edges_count: u64,
}

impl FlowSnapshot {
    pub const fn new(current_time: Duration, current_edges_count: u64) -> Self {
        Self {
            time: current_time,
            edges_count: current_edges_count,
        }
    }

    /// Get a reference to the flow snapshot's time.
    pub fn time(&self) -> Duration {
        self.time
    }

    /// Get a reference to the flow snapshot's edges count.
    pub fn edges_count(&self) -> u64 {
        self.edges_count
    }

    pub fn is_measurement_due(
        &self,
        measurement_duration: Duration,
        current_time: Duration,
    ) -> bool {
        Self::is_aligned_measurement_due(self.time, current_time, measurement_duration)
    }

    pub fn flow_detected(&self, current_edges_count: u64) -> bool {
        self.statistics(current_edges_count) > 1
    }

    pub fn statistics(&self, current_edges_count: u64) -> u64 {
        current_edges_count - self.edges_count
    }

    fn is_nonaligned_measurement_due(
        start_time: Duration,
        current_time: Duration,
        measurement_duration: Duration,
    ) -> bool {
        current_time - start_time >= measurement_duration
    }

    fn is_aligned_measurement_due(
        start_time: Duration,
        current_time: Duration,
        measurement_duration: Duration,
    ) -> bool {
        let start_time = Duration::from_secs(
            start_time.as_secs() / measurement_duration.as_secs() * measurement_duration.as_secs(),
        );

        Self::is_nonaligned_measurement_due(start_time, current_time, measurement_duration)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FlowMeasurement {
    start: FlowSnapshot,
    end: FlowSnapshot,
}

impl FlowMeasurement {
    pub const fn new(start: FlowSnapshot, end: FlowSnapshot) -> Self {
        Self { start, end }
    }

    pub fn start(&self) -> &FlowSnapshot {
        &self.start
    }

    pub fn end(&self) -> &FlowSnapshot {
        &self.end
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WaterMeterStatsState {
    pub installation: FlowSnapshot,

    pub most_recent: FlowSnapshot,

    pub snapshots: [FlowSnapshot; FLOW_STATS_INSTANCES],
    pub measurements: [Option<FlowMeasurement>; FLOW_STATS_INSTANCES],
}

impl WaterMeterStatsState {
    fn update(&mut self, edges_count: u64, now: Duration) -> bool {
        let most_recent = FlowSnapshot::new(now, self.most_recent.edges_count + edges_count);

        let mut updated = self.most_recent != most_recent;
        if updated {
            self.most_recent = most_recent;
        }

        for (index, snapshot) in self.snapshots.iter_mut().enumerate() {
            if snapshot.is_measurement_due(DURATIONS[index], now) {
                let prev = mem::replace(snapshot, self.most_recent.clone());
                self.measurements[index] =
                    Some(FlowMeasurement::new(prev, self.most_recent.clone()));

                updated = true;
            }
        }

        updated
    }
}

pub struct WaterMeterStats<M>
where
    M: MutexFamily + SendSyncSignalFamily,
{
    state: StateSnapshot<M::Mutex<WaterMeterStatsState>>,
    wm_state_signal: M::Signal<WaterMeterState>,
}

impl<M> WaterMeterStats<M>
where
    M: MutexFamily + SendSyncSignalFamily,
{
    pub fn new() -> Self {
        Self {
            state: StateSnapshot::new(),
            wm_state_signal: M::Signal::new(),
        }
    }

    pub fn state(&self) -> &StateSnapshot<impl Mutex<Data = WaterMeterStatsState>> {
        &self.state
    }

    pub async fn process(
        &'static self,
        timer: impl OnceTimer,
        sys_time: impl SystemTime,
        state_sink: impl Sender<Data = WaterMeterStatsState>,
    ) -> error::Result<()> {
        process(
            timer,
            sys_time,
            &self.state,
            as_static_receiver(&self.wm_state_signal),
            state_sink,
        )
        .await
    }
}

pub async fn process(
    mut timer: impl OnceTimer,
    sys_time: impl SystemTime,
    state: &StateSnapshot<impl Mutex<Data = WaterMeterStatsState>>,
    mut wm_state_source: impl Receiver<Data = WaterMeterState>,
    mut state_sink: impl Sender<Data = WaterMeterStatsState>,
) -> error::Result<()> {
    loop {
        let wm_state = wm_state_source.recv();
        let tick = timer
            .after(Duration::from_secs(10) /*Duration::from_millis(200)*/)
            .map_err(error::svc)?;

        //pin_mut!(wm_state, tick);

        let edges_count = match select(wm_state, tick).await {
            Either::First(wm_state) => wm_state.map_err(error::svc)?.edges_count,
            Either::Second(_) => state.get().most_recent.edges_count,
        };

        state
            .update_with(
                |state| {
                    let mut state = state.clone();

                    state.update(edges_count, sys_time.now());

                    Ok(state)
                },
                &mut state_sink,
            )
            .await?;
    }
}
