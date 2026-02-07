use crate::TestAssertion;
use tonic::async_trait;

#[async_trait]
pub trait InteropTest: Send {
    async fn empty_unary(&mut self, assertions: &mut Vec<TestAssertion>);

    async fn large_unary(&mut self, assertions: &mut Vec<TestAssertion>);

    async fn client_streaming(&mut self, assertions: &mut Vec<TestAssertion>);

    async fn server_streaming(&mut self, assertions: &mut Vec<TestAssertion>);

    async fn ping_pong(&mut self, assertions: &mut Vec<TestAssertion>);

    async fn empty_stream(&mut self, assertions: &mut Vec<TestAssertion>);

    async fn status_code_and_message(&mut self, assertions: &mut Vec<TestAssertion>);

    async fn special_status_message(&mut self, assertions: &mut Vec<TestAssertion>);

    async fn unimplemented_method(&mut self, assertions: &mut Vec<TestAssertion>);

    async fn custom_metadata(&mut self, assertions: &mut Vec<TestAssertion>);
}

#[async_trait]
pub trait InteropTestUnimplemented: Send {
    async fn unimplemented_service(&mut self, assertions: &mut Vec<TestAssertion>);
}
