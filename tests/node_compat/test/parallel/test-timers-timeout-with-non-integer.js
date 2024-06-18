// deno-fmt-ignore-file
// deno-lint-ignore-file

// Copyright Joyent and Node contributors. All rights reserved. MIT license.
// Taken from Node 18.12.1
// This file is automatically generated by `tests/node_compat/runner/setup.ts`. Do not modify this file manually.

'use strict';
const common = require('../common');

/**
 * This test is for https://github.com/nodejs/node/issues/24203
 */
let count = 50;
const time = 1.00000000000001;
const exec = common.mustCall(() => {
  if (--count === 0) {
    return;
  }
  setTimeout(exec, time);
}, count);
exec();
