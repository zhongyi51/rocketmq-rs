use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::Value;
use tracing::error;

use crate::client::{Client, ClientOptions};
use crate::message::{MessageExt, MessageQueue};
use crate::namesrv::NameServer;
use crate::protocol::{
    request::{
        GetConsumerListRequestHeader, GetMaxOffsetRequestHeader,
        SearchOffsetByTimestampRequestHeader,
    },
    RemotingCommand, RequestCode, ResponseCode,
};
use crate::resolver::{HttpResolver, PassthroughResolver, Resolver};
use crate::Error;

mod offset_store;
mod process_queue;
mod push;
/// Message queue allocation strategy
pub mod strategy;

use offset_store::{LocalFileOffsetStore, OffsetStorage, RemoteBrokerOffsetStore};
use process_queue::ProcessQueue;
pub use push::PushConsumer;
use strategy::{AllocateAveragely, AllocateStrategy};
use crate::consumer::offset_store::OffsetStore;
use crate::protocol::RequestCode::GetConsumerListByGroup;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MessageModel {
    BroadCasting,
    Clustering,
}

impl fmt::Display for MessageModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageModel::BroadCasting => write!(f, "BroadCasting"),
            MessageModel::Clustering => write!(f, "Clustering"),
        }
    }
}

/// Consume from where
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConsumeFrom {
    LastOffset,
    FirstOffset,
    Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExpressionType {
    Sql92,
    Tag,
}

impl fmt::Display for ExpressionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExpressionType::Sql92 => write!(f, "SQL92"),
            ExpressionType::Tag => write!(f, "TAG"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConsumerOptions {
    client_options: ClientOptions,
    resolver: Resolver,
    max_reconsume_times: i32,
    consume_timeout: Duration,
    message_model: MessageModel,
    consume_from: ConsumeFrom,
    auto_commit: bool,
}

impl Default for ConsumerOptions {
    fn default() -> Self {
        Self {
            client_options: ClientOptions::default(),
            resolver: Resolver::Http(HttpResolver::new("DEFAULT".to_string())),
            max_reconsume_times: -1,
            consume_timeout: Duration::from_secs(0),
            message_model: MessageModel::Clustering,
            consume_from: ConsumeFrom::LastOffset,
            auto_commit: true,
        }
    }
}

impl ConsumerOptions {
    pub fn set_resolver(&mut self, resolver: Resolver) -> &mut Self {
        self.resolver = resolver;
        self
    }

    pub fn set_name_server(&mut self, addrs: Vec<String>) -> &mut Self {
        self.resolver = Resolver::PassthroughHttp(PassthroughResolver::new(
            addrs,
            HttpResolver::new("DEFAULT".to_string()),
        ));
        self
    }

    pub fn set_name_server_domain(&mut self, url: &str) -> &mut Self {
        self.resolver = Resolver::Http(HttpResolver::with_domain(
            "DEFAULT".to_string(),
            url.to_string(),
        ));
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConsumeResult {
    Success,
    RetryLater,
    Commit,
    Rollback,
    SuspendCurrentQueueAMoment,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConsumerReturn {
    Success,
    Exception,
    Null,
    Timeout,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConsumeType {
    Actively,
    Passively,
}

impl fmt::Display for ConsumeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConsumeType::Actively => write!(f, "CONSUME_ACTIVELY"),
            ConsumeType::Passively => write!(f, "CONSUME_PASSIVELY"),
        }
    }
}

pub(crate) struct SubscriptionData{}

#[derive(Debug)]
pub(crate) struct ConsumerInner {
}

impl ConsumerInner {
    pub fn rebalance(&self) {
        self.subscription_map.iter()
            .for_each(|(topic,sub_data)|{
                match self.model{
                    MessageModel::BroadCasting => {todo!("Support broadcasting msg")}
                    MessageModel::Clustering => {

                    }
                }
            })
    }


}

#[derive(Debug)]
pub struct Consumer {
    consumer_group: String,
    inner: Arc<Mutex<ConsumerInner>>,
    options: ConsumerOptions,
    client: Client<Resolver>,
    storage: OffsetStorage,
    allocate: AllocateStrategy,
    process_queue_map: HashMap<MessageQueue, ProcessQueue>,
    subscription_map:HashMap<String,SubscriptionData>,
    subscription_topic_info_map:HashMap<String,Vec<MessageQueue>>
}

impl Consumer {
    pub fn new() -> Result<Self, Error> {
        Self::with_options(ConsumerOptions::default())
    }

    pub fn with_options(options: ConsumerOptions) -> Result<Self, Error> {
        let client_options = options.client_options.clone();
        let inner = Arc::new(Mutex::new(ConsumerInner{}));
        let name_server =
            NameServer::new(options.resolver.clone(), client_options.credentials.clone())?;
        let client = Client::new(client_options, name_server);
        let consumer_group = &options.client_options.group_name;
        let offset_store = match options.message_model {
            MessageModel::Clustering => OffsetStorage::RemoteBroker(RemoteBrokerOffsetStore::new(
                consumer_group,
                client.clone(),
            )),
            MessageModel::BroadCasting => {
                OffsetStorage::LocalFile(LocalFileOffsetStore::new(consumer_group, &client.id()))
            }
        };
        Ok(Self {
            consumer_group: consumer_group.clone(),
            inner,
            options,
            client,
            storage: offset_store,
            allocate: AllocateStrategy::Averagely(AllocateAveragely),
            process_queue_map: HashMap::new(),
        })
    }

    pub fn start(&self) {
        self.client.start();
    }

    pub fn shutdown(&self) {
        self.client.shutdown();
    }

    async fn get_broker_addr(&self, topic: &str) -> Result<String, Error> {
        match self.client.name_server.find_broker_addr_by_topic(topic) {
            Some(addr) => Ok(addr),
            None => {
                self.client
                    .name_server
                    .update_topic_route_info(topic)
                    .await?;
                match self.client.name_server.find_broker_addr_by_topic(topic) {
                    Some(addr) => Ok(addr),
                    None => Err(Error::EmptyRouteData),
                }
            }
        }
    }

    pub async fn get_consumer_list(&self, topic: &str) -> Result<Vec<String>, Error> {
        let broker_addr = self.get_broker_addr(topic).await?;
        let header = GetConsumerListRequestHeader {
            consumer_group: self.consumer_group.clone(),
        };
        let cmd =
            RemotingCommand::with_header(RequestCode::GetConsumerListByGroup, header, Vec::new());
        match self
            .client
            .invoke_timeout(&broker_addr, cmd, Duration::from_secs(3))
            .await
        {
            Ok(res) => {
                if res.body.is_empty() {
                    return Ok(Vec::new());
                }
                let result: serde_json::Value = serde_json::from_slice(&res.body)?;
                if let Some(list) = result
                    .get("consumerIdList")
                    .and_then(|list| list.as_array())
                {
                    let consumers: Vec<String> = list
                        .iter()
                        .map(|v| v.as_str().map(ToString::to_string).unwrap())
                        .collect();
                    Ok(consumers)
                } else {
                    Ok(Vec::new())
                }
            }
            Err(err) => {
                error!(consumer_group = %self.consumer_group, broker = %broker_addr, "get consumer list of group from broker error: {:?}", err);
                Err(err)
            }
        }
    }

    pub async fn get_max_offset(&self, mq: &MessageQueue) -> Result<i64, Error> {
        let broker_addr = self.get_broker_addr(&mq.topic).await?;
        let header = GetMaxOffsetRequestHeader {
            topic: mq.topic.clone(),
            queue_id: mq.queue_id,
        };
        let cmd = RemotingCommand::with_header(RequestCode::GetMaxOffset, header, Vec::new());
        let res = self
            .client
            .invoke_timeout(&broker_addr, cmd, Duration::from_secs(3))
            .await?;
        if res.code() == ResponseCode::Success {
            let offset: i64 = res
                .header
                .ext_fields
                .get("offset")
                .and_then(|s| s.parse().ok())
                .unwrap();
            Ok(offset)
        } else {
            Err(Error::ResponseError {
                code: res.code(),
                message: res.header.remark,
            })
        }
    }

    pub async fn search_offset_by_timestamp(
        &self,
        mq: &MessageQueue,
        timestamp: i64,
    ) -> Result<i64, Error> {
        let broker_addr = self.get_broker_addr(&mq.topic).await?;
        let header = SearchOffsetByTimestampRequestHeader {
            topic: mq.topic.clone(),
            queue_id: mq.queue_id,
            timestamp,
        };
        let cmd =
            RemotingCommand::with_header(RequestCode::SearchOffsetByTimestamp, header, Vec::new());
        let res = self
            .client
            .invoke_timeout(&broker_addr, cmd, Duration::from_secs(3))
            .await?;
        if res.code() == ResponseCode::Success {
            let offset: i64 = res
                .header
                .ext_fields
                .get("offset")
                .and_then(|s| s.parse().ok())
                .unwrap();
            Ok(offset)
        } else {
            Err(Error::ResponseError {
                code: res.code(),
                message: res.header.remark,
            })
        }
    }

    pub async fn rebalance(&self) {
        for (topic,value) in &self.subscription_map{
                match self.options.message_model{
                    MessageModel::BroadCasting => {todo!("Support broadcasting msg")}
                    MessageModel::Clustering => {
                        if let Ok(mut consume_ls) =self.get_consumer_list(topic).await{
                            consume_ls.sort();
                            if let Some(q)=self.subscription_topic_info_map.get(topic){
                                let mut queues=q.clone();
                                queues.sort_by(|i,j|
                                    i.topic.cmp(&j.topic)
                                        .then(i.broker_name.cmp(&j.broker_name))
                                        .then(i.queue_id.cmp(&j.queue_id)));
                                let alloc_res=self.allocate
                                    .allocate(&self.consumer_group, &self.client.id(),queues.as_slice(),consume_ls.iter().map(|sr|sr.as_str()).collect::<Vec<&str>>().as_slice());
                                todo!()
                            }
                        }
                    }
                }
            }
    }

    // todo!(Support pull consumer)
    async fn update_queue_map(&mut self,topic:&str,queues:&Vec<MessageQueue>)->bool{
        for (mq,pq) in &mut self.process_queue_map{
            if mq.topic.eq(topic){
                if !queues.contains(mq){
                    pq.set_dropped(true);
                    self.storage.persist(vec![mq.clone()].as_slice()).await;
                    self.storage.remove(mq);
                    self.process_queue_map.remove(mq);
                }
            }
        }
        todo!()
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;
    use tokio::time::sleep;
    use crate::consumer::{ConsumeResult, ConsumerReturn};
    use crate::consumer::push::MessageSelector;
    use super::{Consumer, ConsumerOptions};
    use crate::message::{MessageExt, MessageQueue};
    use crate::PushConsumer;

    #[tokio::test]
    async fn test_get_consumer_list() {
        // tracing_subscriber::fmt::init();
        let mut options = ConsumerOptions::default();
        options.set_name_server(vec!["localhost:9876".to_string()]);
        let consumer = Consumer::with_options(options).unwrap();
        let consumer_list = consumer.get_consumer_list("SELF_TEST_TOPIC").await.unwrap();
        assert!(consumer_list.is_empty());
    }

    #[tokio::test]
    async fn test_get_max_offset() {
        // tracing_subscriber::fmt::init();
        let mut options = ConsumerOptions::default();
        options.set_name_server(vec!["localhost:9876".to_string()]);
        let consumer = Consumer::with_options(options).unwrap();
        let mq = MessageQueue {
            topic: "SELF_TEST_TOPIC".to_string(),
            broker_name: String::new(),
            queue_id: 0,
        };
        let offset = consumer.get_max_offset(&mq).await.unwrap();
        assert!(offset >= 0);
    }

    #[tokio::test]
    async fn test_search_offset_by_timestamp() {
        // tracing_subscriber::fmt::init();
        let mut options = ConsumerOptions::default();
        options.set_name_server(vec!["localhost:9876".to_string()]);
        let consumer = Consumer::with_options(options).unwrap();
        let mq = MessageQueue {
            topic: "SELF_TEST_TOPIC".to_string(),
            broker_name: String::new(),
            queue_id: 0,
        };
        let offset = consumer.search_offset_by_timestamp(&mq, 0).await.unwrap();
        assert!(offset >= 0);
    }

    #[tokio::test]
    async fn test_push_consumer(){
        let mut options = ConsumerOptions::default();
        options.set_name_server(vec!["localhost:9876".to_string()]);
        let consumer=PushConsumer::with_options(options).unwrap();
        consumer.start();

        async fn cb(msgs:Vec<MessageExt>)->ConsumeResult{
            sleep(Duration::from_secs(100)).await;
            return ConsumeResult::Success;
        }
        consumer.subscribe("SELC_TEST_TOPIC",MessageSelector::NULL,cb);
    }
}
