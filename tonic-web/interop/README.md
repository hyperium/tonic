## Running interop tests

Start the server:

    cd tonic-grpc-web/interop
    cargo run
        
Run the client tests:
    
     cd tonic-grpc-web/interop/client
     npm i
     npm test -- --mode=binary # runs tests in binary mode (application/grpc-web)
     npm test # runs tests in text mode (application/grpc-web-text)
        
Note that in binary mode, server streaming is not supported and the test is skipped.    
    