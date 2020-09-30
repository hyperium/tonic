## Running interop tests

Start the server:

    cd tonic-web/interop
    cargo run
        
Build the client docker image:
    
     cd tonic-web/interop
     docker build -t grpcweb-client .
     
Run tests on linux:
     
     docker run --network=host --rm grpcweb-client /test.sh
     
Run tests on docker desktop: 
     
     docker run --rm grpcweb-client /test.sh host.docker.internal
