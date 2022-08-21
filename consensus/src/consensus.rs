use crate::{
    model::{
        services::{reachability::MTReachabilityService, relations::MTRelationsService, statuses::MTStatusesService},
        stores::{
            ghostdag::DbGhostdagStore, reachability::DbReachabilityStore, relations::DbRelationsStore,
            statuses::DbStatusesStore, DB,
        },
    },
    params::Params,
    pipeline::{
        header_processor::{BlockTask, HeaderProcessor},
        ProcessingCounters,
    },
    processes::reachability::inquirer as reachability,
};
use consensus_core::block::Block;
use crossbeam_channel::{bounded, Receiver, Sender};
use kaspa_core::{core::Core, service::Service};
use parking_lot::RwLock;
use std::{
    ops::DerefMut,
    sync::Arc,
    thread::{self, JoinHandle},
};

pub struct Consensus {
    // DB
    db: Arc<DB>,

    // Channels
    block_sender: Sender<BlockTask>,

    // Processors
    header_processor: Arc<HeaderProcessor>,

    // Stores
    statuses_store: Arc<RwLock<DbStatusesStore>>,
    relations_store: Arc<RwLock<DbRelationsStore>>,
    reachability_store: Arc<RwLock<DbReachabilityStore>>,

    // Append-only stores
    ghostdag_store: Arc<DbGhostdagStore>,

    // Services
    statuses_service: Arc<MTStatusesService<DbStatusesStore>>,
    relations_service: Arc<MTRelationsService<DbRelationsStore>>,
    reachability_service: Arc<MTReachabilityService<DbReachabilityStore>>,

    // Counters
    pub counters: Arc<ProcessingCounters>,
}

impl Consensus {
    pub fn new(db: Arc<DB>, params: &Params) -> Self {
        let statuses_store = Arc::new(RwLock::new(DbStatusesStore::new(db.clone(), 100000)));
        let relations_store = Arc::new(RwLock::new(DbRelationsStore::new(db.clone(), 100000)));
        let reachability_store = Arc::new(RwLock::new(DbReachabilityStore::new(db.clone(), 100000)));
        let ghostdag_store = Arc::new(DbGhostdagStore::new(db.clone(), 100000));

        let statuses_service = Arc::new(MTStatusesService::new(statuses_store.clone()));
        let relations_service = Arc::new(MTRelationsService::new(relations_store.clone()));
        let reachability_service = Arc::new(MTReachabilityService::new(reachability_store.clone()));

        let (sender, receiver): (Sender<BlockTask>, Receiver<BlockTask>) = bounded(2000);
        let counters = Arc::new(ProcessingCounters::default());

        let header_processor = Arc::new(HeaderProcessor::new(
            receiver,
            params,
            db.clone(),
            relations_store.clone(),
            reachability_store.clone(),
            ghostdag_store.clone(),
            counters.clone(),
        ));

        Self {
            db,
            block_sender: sender,
            header_processor,
            statuses_store,
            relations_store,
            reachability_store,
            ghostdag_store,

            statuses_service,
            relations_service,
            reachability_service,

            counters,
        }
    }

    pub fn init(&self) -> JoinHandle<()> {
        // Ensure that reachability store is initialized
        reachability::init(self.reachability_store.write().deref_mut()).unwrap();

        // Ensure that genesis was processed
        self.header_processor.process_genesis_if_needed();

        // Spawn the asynchronous header processor.
        let header_processor = self.header_processor.clone();
        thread::spawn(move || header_processor.worker())

        // TODO: add block body processor and virtual state processor workers and return a vec of join handles.
    }

    pub fn validate_and_insert_block(&self, block: Arc<Block>) {
        self.block_sender
            .send(BlockTask::Process(block))
            .unwrap();
    }

    pub fn signal_exit(&self) {
        self.block_sender.send(BlockTask::Exit).unwrap();
    }

    /// Drops consensus, and specifically drops sender channels so that
    /// internal workers fold up and can be joined.
    pub fn drop(self) -> (Arc<RwLock<DbReachabilityStore>>, Arc<DbGhostdagStore>) {
        self.signal_exit();
        (self.reachability_store, self.ghostdag_store)
    }
}

impl Service for Consensus {
    fn ident(self: Arc<Consensus>) -> String {
        "consensus".to_owned()
    }

    fn start(self: Arc<Consensus>, core: Arc<Core>) -> Vec<JoinHandle<()>> {
        vec![self.init()]
    }

    fn stop(self: Arc<Consensus>) {
        self.signal_exit()
    }
}