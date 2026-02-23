#[derive(Clone, PartialEq, ::prost::Message)]
pub struct GoogleMessage1 {
    #[prost(string, tag = "1")]
    pub field1: std::string::String,
    #[prost(string, tag = "9")]
    pub field9: std::string::String,
    #[prost(string, tag = "18")]
    pub field18: std::string::String,
    #[prost(bool, tag = "80")]
    pub field80: bool,
    #[prost(bool, tag = "81")]
    pub field81: bool,
    #[prost(int32, tag = "2")]
    pub field2: i32,
    #[prost(int32, tag = "3")]
    pub field3: i32,
    #[prost(int32, tag = "280")]
    pub field280: i32,
    #[prost(int32, tag = "6")]
    pub field6: i32,
    #[prost(int64, tag = "22")]
    pub field22: i64,
    #[prost(string, tag = "4")]
    pub field4: std::string::String,
    #[prost(fixed64, repeated, tag = "5")]
    pub field5: ::std::vec::Vec<u64>,
    #[prost(bool, tag = "59")]
    pub field59: bool,
    #[prost(string, tag = "7")]
    pub field7: std::string::String,
    #[prost(int32, tag = "16")]
    pub field16: i32,
    #[prost(int32, tag = "130")]
    pub field130: i32,
    #[prost(bool, tag = "12")]
    pub field12: bool,
    #[prost(bool, tag = "17")]
    pub field17: bool,
    #[prost(bool, tag = "13")]
    pub field13: bool,
    #[prost(bool, tag = "14")]
    pub field14: bool,
    #[prost(int32, tag = "104")]
    pub field104: i32,
    #[prost(int32, tag = "100")]
    pub field100: i32,
    #[prost(int32, tag = "101")]
    pub field101: i32,
    #[prost(string, tag = "102")]
    pub field102: std::string::String,
    #[prost(string, tag = "103")]
    pub field103: std::string::String,
    #[prost(int32, tag = "29")]
    pub field29: i32,
    #[prost(bool, tag = "30")]
    pub field30: bool,
    #[prost(int32, tag = "60")]
    pub field60: i32,
    #[prost(int32, tag = "271")]
    pub field271: i32,
    #[prost(int32, tag = "272")]
    pub field272: i32,
    #[prost(int32, tag = "150")]
    pub field150: i32,
    #[prost(int32, tag = "23")]
    pub field23: i32,
    #[prost(bool, tag = "24")]
    pub field24: bool,
    #[prost(int32, tag = "25")]
    pub field25: i32,
    #[prost(message, optional, tag = "15")]
    pub field15: ::std::option::Option<GoogleMessage1SubMessage>,
    #[prost(bool, tag = "78")]
    pub field78: bool,
    #[prost(int32, tag = "67")]
    pub field67: i32,
    #[prost(int32, tag = "68")]
    pub field68: i32,
    #[prost(int32, tag = "128")]
    pub field128: i32,
    #[prost(string, tag = "129")]
    pub field129: std::string::String,
    #[prost(int32, tag = "131")]
    pub field131: i32,
}
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct GoogleMessage1SubMessage {
    #[prost(int32, tag = "1")]
    pub field1: i32,
    #[prost(int32, tag = "2")]
    pub field2: i32,
    #[prost(int32, tag = "3")]
    pub field3: i32,
    #[prost(string, tag = "15")]
    pub field15: std::string::String,
    #[prost(bool, tag = "12")]
    pub field12: bool,
    #[prost(int64, tag = "13")]
    pub field13: i64,
    #[prost(int64, tag = "14")]
    pub field14: i64,
    #[prost(int32, tag = "16")]
    pub field16: i32,
    #[prost(int32, tag = "19")]
    pub field19: i32,
    #[prost(bool, tag = "20")]
    pub field20: bool,
    #[prost(bool, tag = "28")]
    pub field28: bool,
    #[prost(fixed64, tag = "21")]
    pub field21: u64,
    #[prost(int32, tag = "22")]
    pub field22: i32,
    #[prost(bool, tag = "23")]
    pub field23: bool,
    #[prost(bool, tag = "206")]
    pub field206: bool,
    #[prost(fixed32, tag = "203")]
    pub field203: u32,
    #[prost(int32, tag = "204")]
    pub field204: i32,
    #[prost(string, tag = "205")]
    pub field205: std::string::String,
    #[prost(uint64, tag = "207")]
    pub field207: u64,
    #[prost(uint64, tag = "300")]
    pub field300: u64,
}
