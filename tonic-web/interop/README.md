## Running interop tests

Start the server:

    cd tonic-web/interop
    cargo run
        
Build the client docker image:
    
     cd tonic-web/interop/client
     docker build -t grpcweb-client .
     
Run tests on linux:
     
     docker run --network=host --rm grpcweb-client npm test 
     docker run --network=host --rm grpcweb-client npm test -- --mode=binary 
     
Run tests on docker desktop: 
     
     docker run --rm grpcweb-client npm test -- --host=host.docker.internal 
     docker run --rm grpcweb-client  npm test -- --host=host.docker.internal --mode=binary 
         