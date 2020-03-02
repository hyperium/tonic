docker build -t envoy:v1 .

docker run -d --name envoy -p 9901:9901 -p 10000:10000 envoy:v1