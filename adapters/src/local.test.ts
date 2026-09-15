import test from 'node:test';
import assert from 'node:assert/strict';
import {local} from './local.js';
import {runWorker} from './worker.js';
test('missing CLI fails without shell execution',async()=>{await assert.rejects(local({action:'inbox'},'/nonexistent/whatsai'),/ENOENT/);});
test('unknown harness is rejected before execution',async()=>{await assert.rejects(runWorker({harness:'other' as any,cwd:'.',prompt:'test'}),/Unsupported/);});
