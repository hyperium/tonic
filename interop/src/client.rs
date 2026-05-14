use crate::TestAssertion;
use tonic::async_trait;

#[derive(Clone)]
pub struct MetadataInterceptor {
    pub metadata: tonic::metadata::MetadataMap,
}

impl tonic::service::Interceptor for MetadataInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        for key_and_val in self.metadata.iter() {
            match key_and_val {
                tonic::metadata::KeyAndValueRef::Ascii(key, val) => {
                    request.metadata_mut().insert(key.clone(), val.clone());
                }
                tonic::metadata::KeyAndValueRef::Binary(key, val) => {
                    request.metadata_mut().insert_bin(key.clone(), val.clone());
                }
            }
        }
        Ok(request)
    }
}

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

    async fn cacheable_unary(&mut self, assertions: &mut Vec<TestAssertion>);

    async fn client_compressed_unary(&mut self, assertions: &mut Vec<TestAssertion>);

    async fn server_compressed_unary(&mut self, assertions: &mut Vec<TestAssertion>);

    async fn cancel_after_begin(&mut self, assertions: &mut Vec<TestAssertion>);

    async fn cancel_after_first_response(&mut self, assertions: &mut Vec<TestAssertion>);

    async fn timeout_on_sleeping_server(&mut self, assertions: &mut Vec<TestAssertion>);
}

#[async_trait]
pub trait InteropTestUnimplemented: Send {
    async fn unimplemented_service(&mut self, assertions: &mut Vec<TestAssertion>);
}
