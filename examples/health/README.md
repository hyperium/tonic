# Health checks

gRPC has a [health checking protocol](https://github.com/grpc/grpc/blob/master/doc/health-checking.md) that defines how health checks for services should be carried out. Tonic supports this protocol with the optional [tonic health crate](https://docs.rs/tonic-health).

This example uses the crate to set up a HealthServer that will run alongside the application service. In order to test it, you may use community tools like [grpc_health_probe](https://github.com/grpc-ecosystem/grpc-health-probe).

For example, running the following bash script:

```bash
while [ true ]; do
./grpc_health_probe -addr=[::1]:50051 -service=helloworld.Greeter
sleep 1
done
```

will show the change in health status of the service over time.