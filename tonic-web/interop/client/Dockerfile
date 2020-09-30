FROM node:12-stretch

RUN apt-get -qq update && apt-get -qq install -y unzip

WORKDIR /tmp

RUN curl -sSL https://github.com/protocolbuffers/protobuf/releases/download/v3.12.2/\
protoc-3.12.2-linux-x86_64.zip -o protoc.zip && \
  unzip -qq protoc.zip && \
  cp ./bin/protoc /usr/local/bin/protoc

RUN curl -sSL https://github.com/grpc/grpc-web/releases/download/1.2.1/\
protoc-gen-grpc-web-1.2.1-linux-x86_64 -o /usr/local/bin/protoc-gen-grpc-web && \
  chmod +x /usr/local/bin/protoc-gen-grpc-web

RUN echo "\nloglevel=error\n" >> $HOME/.npmrc

WORKDIR /grpc-web

COPY ./ ./

RUN echo "\nloglevel=error\n" >> $HOME/.npmrc &&\
    npm install &&\
    mkdir -p binary text &&\
    protoc -I=./proto ./proto/*.proto\
        --js_out=import_style=commonjs:./text\
        --grpc-web_out=import_style=commonjs,mode=grpcwebtext:./text &&\
    protoc -I=./proto ./proto/*.proto\
        --js_out=import_style=commonjs:./binary\
        --grpc-web_out=import_style=commonjs,mode=grpcweb:./binary
