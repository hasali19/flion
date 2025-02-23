use std::io::{Cursor, Write};

use crate::codec::{self, EncodableValue};
use crate::engine::{BinaryMessageHandler, BinaryMessageReply};

pub trait StandardMethodHandler {
    fn handle(&self, method: &str, args: EncodableValue, reply: StandardMethodReply);
}

impl<T: StandardMethodHandler> BinaryMessageHandler for T {
    fn handle(&self, message: &[u8], reply: BinaryMessageReply) {
        let reply = StandardMethodReply(reply);

        let mut cursor = Cursor::new(message);

        let method_name = codec::read_value(&mut cursor).unwrap();
        let method_args = codec::read_value(&mut cursor).unwrap();

        let EncodableValue::Str(method_name) = method_name else {
            tracing::error!("invalid method name: {method_name:?}");
            reply.not_implemented();
            return;
        };

        self.handle(method_name, method_args, reply);
    }
}

pub struct StandardMethodReply(BinaryMessageReply);

impl StandardMethodReply {
    pub fn success(self, value: &EncodableValue) {
        let mut bytes = vec![];
        let mut cursor = Cursor::new(&mut bytes);
        cursor.write_all(&[0]).unwrap();
        codec::write_value(&mut cursor, value).unwrap();
        self.0.send(&bytes);
    }

    pub fn not_implemented(self) {
        self.0.not_implemented();
    }
}
