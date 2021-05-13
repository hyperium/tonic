## Running interop tests

Start the server:

```bash
cd tonic-web/interop
cargo run
```
        
Build the client docker image:

```bash
 cd tonic-web/interop
 docker build -t grpcweb-client .
```
     
Run tests on linux:

```bash
docker run --network=host --rm grpcweb-client /test.sh
```
     
Run tests on docker desktop: 
     
```bash
docker run --rm grpcweb-client /test.sh host.docker.internal
```
