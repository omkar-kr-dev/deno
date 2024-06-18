// deno-fmt-ignore-file
// deno-lint-ignore-file

// Copyright Joyent and Node contributors. All rights reserved. MIT license.
// Taken from Node 18.12.1
// This file is automatically generated by `tests/node_compat/runner/setup.ts`. Do not modify this file manually.

'use strict';
require('../common');
const { Writable } = require('stream');
const { Console } = require('console');

const stream = new Writable({
  write() {
    throw null; // eslint-disable-line no-throw-literal
  }
});

const console = new Console({ stdout: stream });

console.log('test'); // Should not throw
