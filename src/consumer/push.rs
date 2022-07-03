use std::future::Future;
use crate::consumer::ConsumeResult;
use super::{Consumer, ConsumerOptions};
use crate::Error;
use crate::message::MessageExt;

#[derive(Debug)]
pub struct PushConsumer {
    consumer: Consumer,
}

impl PushConsumer {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            consumer: Consumer::new()?,
        })
    }

    pub fn with_options(options: ConsumerOptions) -> Result<Self, Error> {
        Ok(Self {
            consumer: Consumer::with_options(options)?,
        })
    }

    pub fn start(&self) {
        todo!()
    }

    pub fn shutdown(&self) {
        todo!()
    }

    pub fn subscribe<F, B>(&self, topic: &str, selector: MessageSelector, callback: F)
        where F: Fn(Vec<MessageExt>) -> B,
              B: Future<Output=ConsumeResult>
    {
        todo!()
    }
}

#[derive(Debug)]
pub enum MessageSelector {
    SQL92(String),
    TAG(String),
    NULL
}


