const path = require('path');

module.exports = {
  entry: './src/client.js',
  output: {
    filename: 'main.js',
    path: path.resolve(__dirname, 'dist'),
  },
  optimization: {
    minimize: false
  },
  mode: 'development',
  devtool: 'source-map'
};