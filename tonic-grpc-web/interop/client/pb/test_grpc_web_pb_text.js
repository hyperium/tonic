/**
 * @fileoverview gRPC-Web generated client stub for grpc.testing
 * @enhanceable
 * @public
 */

// GENERATED CODE -- DO NOT EDIT!


/* eslint-disable */
// @ts-nocheck



const grpc = {};
grpc.web = require('grpc-web');


var grpc_testing_empty_pb = require('./empty_pb.js')

var grpc_testing_messages_pb = require('./messages_pb.js')
const proto = {};
proto.grpc = {};
proto.grpc.testing = require('./test_pb.js');

/**
 * @param {string} hostname
 * @param {?Object} credentials
 * @param {?Object} options
 * @constructor
 * @struct
 * @final
 */
proto.grpc.testing.TestServiceClient =
    function(hostname, credentials, options) {
  if (!options) options = {};
  options['format'] = 'text';

  /**
   * @private @const {!grpc.web.GrpcWebClientBase} The client
   */
  this.client_ = new grpc.web.GrpcWebClientBase(options);

  /**
   * @private @const {string} The hostname
   */
  this.hostname_ = hostname;

};


/**
 * @param {string} hostname
 * @param {?Object} credentials
 * @param {?Object} options
 * @constructor
 * @struct
 * @final
 */
proto.grpc.testing.TestServicePromiseClient =
    function(hostname, credentials, options) {
  if (!options) options = {};
  options['format'] = 'text';

  /**
   * @private @const {!grpc.web.GrpcWebClientBase} The client
   */
  this.client_ = new grpc.web.GrpcWebClientBase(options);

  /**
   * @private @const {string} The hostname
   */
  this.hostname_ = hostname;

};


/**
 * @const
 * @type {!grpc.web.MethodDescriptor<
 *   !proto.grpc.testing.Empty,
 *   !proto.grpc.testing.Empty>}
 */
const methodDescriptor_TestService_EmptyCall = new grpc.web.MethodDescriptor(
  '/grpc.testing.TestService/EmptyCall',
  grpc.web.MethodType.UNARY,
  grpc_testing_empty_pb.Empty,
  grpc_testing_empty_pb.Empty,
  /**
   * @param {!proto.grpc.testing.Empty} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_empty_pb.Empty.deserializeBinary
);


/**
 * @const
 * @type {!grpc.web.AbstractClientBase.MethodInfo<
 *   !proto.grpc.testing.Empty,
 *   !proto.grpc.testing.Empty>}
 */
const methodInfo_TestService_EmptyCall = new grpc.web.AbstractClientBase.MethodInfo(
  grpc_testing_empty_pb.Empty,
  /**
   * @param {!proto.grpc.testing.Empty} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_empty_pb.Empty.deserializeBinary
);


/**
 * @param {!proto.grpc.testing.Empty} request The
 *     request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @param {function(?grpc.web.Error, ?proto.grpc.testing.Empty)}
 *     callback The callback function(error, response)
 * @return {!grpc.web.ClientReadableStream<!proto.grpc.testing.Empty>|undefined}
 *     The XHR Node Readable Stream
 */
proto.grpc.testing.TestServiceClient.prototype.emptyCall =
    function(request, metadata, callback) {
  return this.client_.rpcCall(this.hostname_ +
      '/grpc.testing.TestService/EmptyCall',
      request,
      metadata || {},
      methodDescriptor_TestService_EmptyCall,
      callback);
};


/**
 * @param {!proto.grpc.testing.Empty} request The
 *     request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @return {!Promise<!proto.grpc.testing.Empty>}
 *     Promise that resolves to the response
 */
proto.grpc.testing.TestServicePromiseClient.prototype.emptyCall =
    function(request, metadata) {
  return this.client_.unaryCall(this.hostname_ +
      '/grpc.testing.TestService/EmptyCall',
      request,
      metadata || {},
      methodDescriptor_TestService_EmptyCall);
};


/**
 * @const
 * @type {!grpc.web.MethodDescriptor<
 *   !proto.grpc.testing.SimpleRequest,
 *   !proto.grpc.testing.SimpleResponse>}
 */
const methodDescriptor_TestService_UnaryCall = new grpc.web.MethodDescriptor(
  '/grpc.testing.TestService/UnaryCall',
  grpc.web.MethodType.UNARY,
  grpc_testing_messages_pb.SimpleRequest,
  grpc_testing_messages_pb.SimpleResponse,
  /**
   * @param {!proto.grpc.testing.SimpleRequest} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_messages_pb.SimpleResponse.deserializeBinary
);


/**
 * @const
 * @type {!grpc.web.AbstractClientBase.MethodInfo<
 *   !proto.grpc.testing.SimpleRequest,
 *   !proto.grpc.testing.SimpleResponse>}
 */
const methodInfo_TestService_UnaryCall = new grpc.web.AbstractClientBase.MethodInfo(
  grpc_testing_messages_pb.SimpleResponse,
  /**
   * @param {!proto.grpc.testing.SimpleRequest} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_messages_pb.SimpleResponse.deserializeBinary
);


/**
 * @param {!proto.grpc.testing.SimpleRequest} request The
 *     request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @param {function(?grpc.web.Error, ?proto.grpc.testing.SimpleResponse)}
 *     callback The callback function(error, response)
 * @return {!grpc.web.ClientReadableStream<!proto.grpc.testing.SimpleResponse>|undefined}
 *     The XHR Node Readable Stream
 */
proto.grpc.testing.TestServiceClient.prototype.unaryCall =
    function(request, metadata, callback) {
  return this.client_.rpcCall(this.hostname_ +
      '/grpc.testing.TestService/UnaryCall',
      request,
      metadata || {},
      methodDescriptor_TestService_UnaryCall,
      callback);
};


/**
 * @param {!proto.grpc.testing.SimpleRequest} request The
 *     request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @return {!Promise<!proto.grpc.testing.SimpleResponse>}
 *     Promise that resolves to the response
 */
proto.grpc.testing.TestServicePromiseClient.prototype.unaryCall =
    function(request, metadata) {
  return this.client_.unaryCall(this.hostname_ +
      '/grpc.testing.TestService/UnaryCall',
      request,
      metadata || {},
      methodDescriptor_TestService_UnaryCall);
};


/**
 * @const
 * @type {!grpc.web.MethodDescriptor<
 *   !proto.grpc.testing.SimpleRequest,
 *   !proto.grpc.testing.SimpleResponse>}
 */
const methodDescriptor_TestService_CacheableUnaryCall = new grpc.web.MethodDescriptor(
  '/grpc.testing.TestService/CacheableUnaryCall',
  grpc.web.MethodType.UNARY,
  grpc_testing_messages_pb.SimpleRequest,
  grpc_testing_messages_pb.SimpleResponse,
  /**
   * @param {!proto.grpc.testing.SimpleRequest} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_messages_pb.SimpleResponse.deserializeBinary
);


/**
 * @const
 * @type {!grpc.web.AbstractClientBase.MethodInfo<
 *   !proto.grpc.testing.SimpleRequest,
 *   !proto.grpc.testing.SimpleResponse>}
 */
const methodInfo_TestService_CacheableUnaryCall = new grpc.web.AbstractClientBase.MethodInfo(
  grpc_testing_messages_pb.SimpleResponse,
  /**
   * @param {!proto.grpc.testing.SimpleRequest} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_messages_pb.SimpleResponse.deserializeBinary
);


/**
 * @param {!proto.grpc.testing.SimpleRequest} request The
 *     request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @param {function(?grpc.web.Error, ?proto.grpc.testing.SimpleResponse)}
 *     callback The callback function(error, response)
 * @return {!grpc.web.ClientReadableStream<!proto.grpc.testing.SimpleResponse>|undefined}
 *     The XHR Node Readable Stream
 */
proto.grpc.testing.TestServiceClient.prototype.cacheableUnaryCall =
    function(request, metadata, callback) {
  return this.client_.rpcCall(this.hostname_ +
      '/grpc.testing.TestService/CacheableUnaryCall',
      request,
      metadata || {},
      methodDescriptor_TestService_CacheableUnaryCall,
      callback);
};


/**
 * @param {!proto.grpc.testing.SimpleRequest} request The
 *     request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @return {!Promise<!proto.grpc.testing.SimpleResponse>}
 *     Promise that resolves to the response
 */
proto.grpc.testing.TestServicePromiseClient.prototype.cacheableUnaryCall =
    function(request, metadata) {
  return this.client_.unaryCall(this.hostname_ +
      '/grpc.testing.TestService/CacheableUnaryCall',
      request,
      metadata || {},
      methodDescriptor_TestService_CacheableUnaryCall);
};


/**
 * @const
 * @type {!grpc.web.MethodDescriptor<
 *   !proto.grpc.testing.StreamingOutputCallRequest,
 *   !proto.grpc.testing.StreamingOutputCallResponse>}
 */
const methodDescriptor_TestService_StreamingOutputCall = new grpc.web.MethodDescriptor(
  '/grpc.testing.TestService/StreamingOutputCall',
  grpc.web.MethodType.SERVER_STREAMING,
  grpc_testing_messages_pb.StreamingOutputCallRequest,
  grpc_testing_messages_pb.StreamingOutputCallResponse,
  /**
   * @param {!proto.grpc.testing.StreamingOutputCallRequest} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_messages_pb.StreamingOutputCallResponse.deserializeBinary
);


/**
 * @const
 * @type {!grpc.web.AbstractClientBase.MethodInfo<
 *   !proto.grpc.testing.StreamingOutputCallRequest,
 *   !proto.grpc.testing.StreamingOutputCallResponse>}
 */
const methodInfo_TestService_StreamingOutputCall = new grpc.web.AbstractClientBase.MethodInfo(
  grpc_testing_messages_pb.StreamingOutputCallResponse,
  /**
   * @param {!proto.grpc.testing.StreamingOutputCallRequest} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_messages_pb.StreamingOutputCallResponse.deserializeBinary
);


/**
 * @param {!proto.grpc.testing.StreamingOutputCallRequest} request The request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @return {!grpc.web.ClientReadableStream<!proto.grpc.testing.StreamingOutputCallResponse>}
 *     The XHR Node Readable Stream
 */
proto.grpc.testing.TestServiceClient.prototype.streamingOutputCall =
    function(request, metadata) {
  return this.client_.serverStreaming(this.hostname_ +
      '/grpc.testing.TestService/StreamingOutputCall',
      request,
      metadata || {},
      methodDescriptor_TestService_StreamingOutputCall);
};


/**
 * @param {!proto.grpc.testing.StreamingOutputCallRequest} request The request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @return {!grpc.web.ClientReadableStream<!proto.grpc.testing.StreamingOutputCallResponse>}
 *     The XHR Node Readable Stream
 */
proto.grpc.testing.TestServicePromiseClient.prototype.streamingOutputCall =
    function(request, metadata) {
  return this.client_.serverStreaming(this.hostname_ +
      '/grpc.testing.TestService/StreamingOutputCall',
      request,
      metadata || {},
      methodDescriptor_TestService_StreamingOutputCall);
};


/**
 * @const
 * @type {!grpc.web.MethodDescriptor<
 *   !proto.grpc.testing.Empty,
 *   !proto.grpc.testing.Empty>}
 */
const methodDescriptor_TestService_UnimplementedCall = new grpc.web.MethodDescriptor(
  '/grpc.testing.TestService/UnimplementedCall',
  grpc.web.MethodType.UNARY,
  grpc_testing_empty_pb.Empty,
  grpc_testing_empty_pb.Empty,
  /**
   * @param {!proto.grpc.testing.Empty} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_empty_pb.Empty.deserializeBinary
);


/**
 * @const
 * @type {!grpc.web.AbstractClientBase.MethodInfo<
 *   !proto.grpc.testing.Empty,
 *   !proto.grpc.testing.Empty>}
 */
const methodInfo_TestService_UnimplementedCall = new grpc.web.AbstractClientBase.MethodInfo(
  grpc_testing_empty_pb.Empty,
  /**
   * @param {!proto.grpc.testing.Empty} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_empty_pb.Empty.deserializeBinary
);


/**
 * @param {!proto.grpc.testing.Empty} request The
 *     request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @param {function(?grpc.web.Error, ?proto.grpc.testing.Empty)}
 *     callback The callback function(error, response)
 * @return {!grpc.web.ClientReadableStream<!proto.grpc.testing.Empty>|undefined}
 *     The XHR Node Readable Stream
 */
proto.grpc.testing.TestServiceClient.prototype.unimplementedCall =
    function(request, metadata, callback) {
  return this.client_.rpcCall(this.hostname_ +
      '/grpc.testing.TestService/UnimplementedCall',
      request,
      metadata || {},
      methodDescriptor_TestService_UnimplementedCall,
      callback);
};


/**
 * @param {!proto.grpc.testing.Empty} request The
 *     request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @return {!Promise<!proto.grpc.testing.Empty>}
 *     Promise that resolves to the response
 */
proto.grpc.testing.TestServicePromiseClient.prototype.unimplementedCall =
    function(request, metadata) {
  return this.client_.unaryCall(this.hostname_ +
      '/grpc.testing.TestService/UnimplementedCall',
      request,
      metadata || {},
      methodDescriptor_TestService_UnimplementedCall);
};


/**
 * @param {string} hostname
 * @param {?Object} credentials
 * @param {?Object} options
 * @constructor
 * @struct
 * @final
 */
proto.grpc.testing.UnimplementedServiceClient =
    function(hostname, credentials, options) {
  if (!options) options = {};
  options['format'] = 'text';

  /**
   * @private @const {!grpc.web.GrpcWebClientBase} The client
   */
  this.client_ = new grpc.web.GrpcWebClientBase(options);

  /**
   * @private @const {string} The hostname
   */
  this.hostname_ = hostname;

};


/**
 * @param {string} hostname
 * @param {?Object} credentials
 * @param {?Object} options
 * @constructor
 * @struct
 * @final
 */
proto.grpc.testing.UnimplementedServicePromiseClient =
    function(hostname, credentials, options) {
  if (!options) options = {};
  options['format'] = 'text';

  /**
   * @private @const {!grpc.web.GrpcWebClientBase} The client
   */
  this.client_ = new grpc.web.GrpcWebClientBase(options);

  /**
   * @private @const {string} The hostname
   */
  this.hostname_ = hostname;

};


/**
 * @const
 * @type {!grpc.web.MethodDescriptor<
 *   !proto.grpc.testing.Empty,
 *   !proto.grpc.testing.Empty>}
 */
const methodDescriptor_UnimplementedService_UnimplementedCall = new grpc.web.MethodDescriptor(
  '/grpc.testing.UnimplementedService/UnimplementedCall',
  grpc.web.MethodType.UNARY,
  grpc_testing_empty_pb.Empty,
  grpc_testing_empty_pb.Empty,
  /**
   * @param {!proto.grpc.testing.Empty} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_empty_pb.Empty.deserializeBinary
);


/**
 * @const
 * @type {!grpc.web.AbstractClientBase.MethodInfo<
 *   !proto.grpc.testing.Empty,
 *   !proto.grpc.testing.Empty>}
 */
const methodInfo_UnimplementedService_UnimplementedCall = new grpc.web.AbstractClientBase.MethodInfo(
  grpc_testing_empty_pb.Empty,
  /**
   * @param {!proto.grpc.testing.Empty} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_empty_pb.Empty.deserializeBinary
);


/**
 * @param {!proto.grpc.testing.Empty} request The
 *     request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @param {function(?grpc.web.Error, ?proto.grpc.testing.Empty)}
 *     callback The callback function(error, response)
 * @return {!grpc.web.ClientReadableStream<!proto.grpc.testing.Empty>|undefined}
 *     The XHR Node Readable Stream
 */
proto.grpc.testing.UnimplementedServiceClient.prototype.unimplementedCall =
    function(request, metadata, callback) {
  return this.client_.rpcCall(this.hostname_ +
      '/grpc.testing.UnimplementedService/UnimplementedCall',
      request,
      metadata || {},
      methodDescriptor_UnimplementedService_UnimplementedCall,
      callback);
};


/**
 * @param {!proto.grpc.testing.Empty} request The
 *     request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @return {!Promise<!proto.grpc.testing.Empty>}
 *     Promise that resolves to the response
 */
proto.grpc.testing.UnimplementedServicePromiseClient.prototype.unimplementedCall =
    function(request, metadata) {
  return this.client_.unaryCall(this.hostname_ +
      '/grpc.testing.UnimplementedService/UnimplementedCall',
      request,
      metadata || {},
      methodDescriptor_UnimplementedService_UnimplementedCall);
};


/**
 * @param {string} hostname
 * @param {?Object} credentials
 * @param {?Object} options
 * @constructor
 * @struct
 * @final
 */
proto.grpc.testing.ReconnectServiceClient =
    function(hostname, credentials, options) {
  if (!options) options = {};
  options['format'] = 'text';

  /**
   * @private @const {!grpc.web.GrpcWebClientBase} The client
   */
  this.client_ = new grpc.web.GrpcWebClientBase(options);

  /**
   * @private @const {string} The hostname
   */
  this.hostname_ = hostname;

};


/**
 * @param {string} hostname
 * @param {?Object} credentials
 * @param {?Object} options
 * @constructor
 * @struct
 * @final
 */
proto.grpc.testing.ReconnectServicePromiseClient =
    function(hostname, credentials, options) {
  if (!options) options = {};
  options['format'] = 'text';

  /**
   * @private @const {!grpc.web.GrpcWebClientBase} The client
   */
  this.client_ = new grpc.web.GrpcWebClientBase(options);

  /**
   * @private @const {string} The hostname
   */
  this.hostname_ = hostname;

};


/**
 * @const
 * @type {!grpc.web.MethodDescriptor<
 *   !proto.grpc.testing.ReconnectParams,
 *   !proto.grpc.testing.Empty>}
 */
const methodDescriptor_ReconnectService_Start = new grpc.web.MethodDescriptor(
  '/grpc.testing.ReconnectService/Start',
  grpc.web.MethodType.UNARY,
  grpc_testing_messages_pb.ReconnectParams,
  grpc_testing_empty_pb.Empty,
  /**
   * @param {!proto.grpc.testing.ReconnectParams} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_empty_pb.Empty.deserializeBinary
);


/**
 * @const
 * @type {!grpc.web.AbstractClientBase.MethodInfo<
 *   !proto.grpc.testing.ReconnectParams,
 *   !proto.grpc.testing.Empty>}
 */
const methodInfo_ReconnectService_Start = new grpc.web.AbstractClientBase.MethodInfo(
  grpc_testing_empty_pb.Empty,
  /**
   * @param {!proto.grpc.testing.ReconnectParams} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_empty_pb.Empty.deserializeBinary
);


/**
 * @param {!proto.grpc.testing.ReconnectParams} request The
 *     request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @param {function(?grpc.web.Error, ?proto.grpc.testing.Empty)}
 *     callback The callback function(error, response)
 * @return {!grpc.web.ClientReadableStream<!proto.grpc.testing.Empty>|undefined}
 *     The XHR Node Readable Stream
 */
proto.grpc.testing.ReconnectServiceClient.prototype.start =
    function(request, metadata, callback) {
  return this.client_.rpcCall(this.hostname_ +
      '/grpc.testing.ReconnectService/Start',
      request,
      metadata || {},
      methodDescriptor_ReconnectService_Start,
      callback);
};


/**
 * @param {!proto.grpc.testing.ReconnectParams} request The
 *     request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @return {!Promise<!proto.grpc.testing.Empty>}
 *     Promise that resolves to the response
 */
proto.grpc.testing.ReconnectServicePromiseClient.prototype.start =
    function(request, metadata) {
  return this.client_.unaryCall(this.hostname_ +
      '/grpc.testing.ReconnectService/Start',
      request,
      metadata || {},
      methodDescriptor_ReconnectService_Start);
};


/**
 * @const
 * @type {!grpc.web.MethodDescriptor<
 *   !proto.grpc.testing.Empty,
 *   !proto.grpc.testing.ReconnectInfo>}
 */
const methodDescriptor_ReconnectService_Stop = new grpc.web.MethodDescriptor(
  '/grpc.testing.ReconnectService/Stop',
  grpc.web.MethodType.UNARY,
  grpc_testing_empty_pb.Empty,
  grpc_testing_messages_pb.ReconnectInfo,
  /**
   * @param {!proto.grpc.testing.Empty} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_messages_pb.ReconnectInfo.deserializeBinary
);


/**
 * @const
 * @type {!grpc.web.AbstractClientBase.MethodInfo<
 *   !proto.grpc.testing.Empty,
 *   !proto.grpc.testing.ReconnectInfo>}
 */
const methodInfo_ReconnectService_Stop = new grpc.web.AbstractClientBase.MethodInfo(
  grpc_testing_messages_pb.ReconnectInfo,
  /**
   * @param {!proto.grpc.testing.Empty} request
   * @return {!Uint8Array}
   */
  function(request) {
    return request.serializeBinary();
  },
  grpc_testing_messages_pb.ReconnectInfo.deserializeBinary
);


/**
 * @param {!proto.grpc.testing.Empty} request The
 *     request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @param {function(?grpc.web.Error, ?proto.grpc.testing.ReconnectInfo)}
 *     callback The callback function(error, response)
 * @return {!grpc.web.ClientReadableStream<!proto.grpc.testing.ReconnectInfo>|undefined}
 *     The XHR Node Readable Stream
 */
proto.grpc.testing.ReconnectServiceClient.prototype.stop =
    function(request, metadata, callback) {
  return this.client_.rpcCall(this.hostname_ +
      '/grpc.testing.ReconnectService/Stop',
      request,
      metadata || {},
      methodDescriptor_ReconnectService_Stop,
      callback);
};


/**
 * @param {!proto.grpc.testing.Empty} request The
 *     request proto
 * @param {?Object<string, string>} metadata User defined
 *     call metadata
 * @return {!Promise<!proto.grpc.testing.ReconnectInfo>}
 *     Promise that resolves to the response
 */
proto.grpc.testing.ReconnectServicePromiseClient.prototype.stop =
    function(request, metadata) {
  return this.client_.unaryCall(this.hostname_ +
      '/grpc.testing.ReconnectService/Stop',
      request,
      metadata || {},
      methodDescriptor_ReconnectService_Stop);
};


module.exports = proto.grpc.testing;

