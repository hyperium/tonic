/**
 *
 * Copyright 2018 Google LLC
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
 */

// Adapted from https://github.com/grpc/grpc-web/tree/master/test/interop

global.XMLHttpRequest = require("xhr2");

const parseArgs = require('minimist');
const argv = parseArgs(process.argv, {
    string: ['mode', 'host']
});

const SERVER_HOST = `http://${argv.host || "localhost"}:9999`;

if (argv.mode === 'binary') {
    console.log('Testing tonic-web mode (binary)...');
} else {
    console.log('Testing tonic-web mode (text)...');
}
console.log('Tonic server:', SERVER_HOST);

const PROTO_PATH = argv.mode === 'binary' ? './binary' : './text';

const {
    Empty,
    SimpleRequest,
    StreamingOutputCallRequest,
    EchoStatus,
    Payload,
    ResponseParameters
} = require(`${PROTO_PATH}/test_pb.js`);

const {TestServiceClient} = require(`${PROTO_PATH}/test_grpc_web_pb.js`);

const assert = require('assert');
const grpc = {};
grpc.web = require('grpc-web');

function multiDone(done, count) {
    return function () {
        count -= 1;
        if (count <= 0) {
            done();
        }
    };
}

function doEmptyUnary(done) {
    const testService = new TestServiceClient(SERVER_HOST, null, null);
    testService.emptyCall(new Empty(), null, (err, response) => {
        assert.ifError(err);
        assert(response instanceof Empty);
        done();
    });
}

function doLargeUnary(done) {
    const testService = new TestServiceClient(SERVER_HOST, null, null);
    const req = new SimpleRequest();
    const size = 314159;

    const payload = new Payload();
    payload.setBody('0'.repeat(271828));

    req.setPayload(payload);
    req.setResponseSize(size);

    testService.unaryCall(req, null, (err, response) => {
        assert.ifError(err);
        assert.equal(response.getPayload().getBody().length, size);
        done();
    });
}

function doServerStreaming(done) {
    const testService = new TestServiceClient(SERVER_HOST, null, null);
    const sizes = [31415, 9, 2653, 58979];

    const responseParams = sizes.map((size, idx) => {
        const param = new ResponseParameters();
        param.setSize(size);
        param.setIntervalUs(idx * 10);
        return param;
    });

    const req = new StreamingOutputCallRequest();
    req.setResponseParametersList(responseParams);

    const stream = testService.streamingOutputCall(req);

    done = multiDone(done, sizes.length);
    let numCallbacks = 0;
    stream.on('data', (response) => {
        assert.equal(response.getPayload().getBody().length, sizes[numCallbacks]);
        numCallbacks++;
        done();
    });
}

function doCustomMetadata(done) {
    const testService = new TestServiceClient(SERVER_HOST, null, null);
    done = multiDone(done, 3);

    const req = new SimpleRequest();
    const size = 314159;
    const ECHO_INITIAL_KEY = 'x-grpc-test-echo-initial';
    const ECHO_INITIAL_VALUE = 'test_initial_metadata_value';
    const ECHO_TRAILING_KEY = 'x-grpc-test-echo-trailing-bin';
    const ECHO_TRAILING_VALUE = 0xababab;

    const payload = new Payload();
    payload.setBody('0'.repeat(271828));

    req.setPayload(payload);
    req.setResponseSize(size);

    const call = testService.unaryCall(req, {
        [ECHO_INITIAL_KEY]: ECHO_INITIAL_VALUE,
        [ECHO_TRAILING_KEY]: ECHO_TRAILING_VALUE
    }, (err, response) => {
        assert.ifError(err);
        assert.equal(response.getPayload().getBody().length, size);
        done();
    });

    call.on('metadata', (metadata) => {
        assert(ECHO_INITIAL_KEY in metadata);
        assert.equal(metadata[ECHO_INITIAL_KEY], ECHO_INITIAL_VALUE);
        done();
    });

    call.on('status', (status) => {
        assert('metadata' in status);
        assert(ECHO_TRAILING_KEY in status.metadata);
        assert.equal(status.metadata[ECHO_TRAILING_KEY], ECHO_TRAILING_VALUE);
        done();
    });
}

function doStatusCodeAndMessage(done) {
    const testService = new TestServiceClient(SERVER_HOST, null, null);
    const req = new SimpleRequest();

    const TEST_STATUS_MESSAGE = 'test status message';
    const echoStatus = new EchoStatus();
    echoStatus.setCode(2);
    echoStatus.setMessage(TEST_STATUS_MESSAGE);

    req.setResponseStatus(echoStatus);

    testService.unaryCall(req, {}, (err, response) => {
        assert(err);
        assert('code' in err);
        assert('message' in err);
        assert.equal(err.code, 2);
        assert.equal(err.message, TEST_STATUS_MESSAGE);
        done();
    });
}

function doUnimplementedMethod(done) {
    const testService = new TestServiceClient(SERVER_HOST, null, null);
    testService.unimplementedCall(new Empty(), {}, (err, response) => {
        assert(err);
        assert('code' in err);
        assert.equal(err.code, 12);
        done();
    });
}

const testCases = {
    'empty_unary': {testFunc: doEmptyUnary},
    'large_unary': {testFunc: doLargeUnary},
    'server_streaming': {
        testFunc: doServerStreaming,
        skipBinaryMode: true
    },
    'custom_metadata': {testFunc: doCustomMetadata},
    'status_code_and_message': {testFunc: doStatusCodeAndMessage},
    'unimplemented_method': {testFunc: doUnimplementedMethod}
};


describe('tonic-web interop tests', function () {
    Object.keys(testCases).forEach((testCase) => {
        if (argv.mode === 'binary' && testCases[testCase].skipBinaryMode) return;
        it('should pass ' + testCase, testCases[testCase].testFunc);
    });
});
