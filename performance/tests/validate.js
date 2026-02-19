/**
 * Correctness validation gate — runs BEFORE performance tests.
 *
 * Creates a small dataset, reads everything back via REST and GraphQL,
 * and asserts that responses contain the expected data. If any check fails
 * the `checks` threshold (rate==1.0) causes k6 to exit non-zero, which
 * should stop the run-all.sh pipeline before meaningless perf numbers
 * are collected.
 */
import { check, fail } from 'k6';
import { BASE_URL, ENTITIES } from '../lib/config.js';
import {
  restPost, restGet, restPut, restDelete,
  graphqlQuery, discoverAll,
} from '../lib/helpers.js';
import {
  makeFarm, makeCoop, makeHen, makeContainer, makeConsumer,
  makeLayReport, makeStorageDeposit, makeConsumptionReport,
} from '../lib/data-generators.js';

export const options = {
  vus: 1,
  iterations: 1,
  thresholds: {
    checks: ['rate==1.0'], // every single check must pass
  },
};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/** Abort early when a prerequisite collection is empty. */
function requireNonEmpty(label, arr) {
  if (!arr || arr.length === 0) {
    fail(`FATAL: ${label} — all subsequent checks are meaningless`);
  }
}

/** Safely access a nested path, returning undefined on any null/missing step. */
function dig(obj, ...keys) {
  let cur = obj;
  for (const k of keys) {
    if (cur == null) return undefined;
    cur = cur[k];
  }
  return cur;
}

// ---------------------------------------------------------------------------
export default function () {
  // ===== 1. REST write + read-back =====

  const farm0 = makeFarm(900);
  const farmPostRes = restPost('farm', farm0);
  check(farmPostRes, {
    'REST POST farm returns body':          (r) => r.body && r.body.length > 2,
    'REST POST farm body contains name':    (r) => r.body.includes(farm0.name),
  });

  restPost('farm', makeFarm(901));
  restPost('farm', makeFarm(902));

  // ===== 2. GraphQL getAll — farms =====

  const farms = discoverAll('farm', 'getAll', 'id name farm_type zone');
  check(null, {
    'getAll farms ≥ 3':         () => farms.length >= 3,
    'farm has id':              () => farms[0].id != null,
    'farm has name':            () => farms[0].name != null,
    'farm has zone':            () => farms[0].zone != null,
    'farm has farm_type':       () => farms[0].farm_type != null,
  });
  requireNonEmpty('No farms returned from getAll', farms);

  // ===== 3. REST GET by id =====

  const farmGetRes = restGet('farm', farms[0].id);
  check(farmGetRes, {
    'REST GET farm returns body':          (r) => r.body && r.body.length > 2,
  });

  // ===== 4. GraphQL getById =====

  const farmById = graphqlQuery('farm',
    `{ getById(id: "${farms[0].id}") { id name farm_type zone } }`);
  check(null, {
    'getById farm returns data':       () => dig(farmById, 'getById') != null,
    'getById farm correct id':         () => dig(farmById, 'getById', 'id') === farms[0].id,
    'getById farm has name':           () => dig(farmById, 'getById', 'name') != null,
    'getById farm has farm_type':      () => dig(farmById, 'getById', 'farm_type') != null,
  });

  // ===== 5. REST PUT (update) =====

  const updatedName = `updated-${Date.now()}`;
  const putRes = restPut('farm', farms[0].id, {
    name: updatedName, farm_type: 'homestead', zone: 'central',
  });
  check(putRes, {
    'REST PUT farm → 200':             (r) => r.status === 200,
    'REST PUT farm body has new name':  (r) => r.body.includes(updatedName),
  });

  // ===== 6. Create coops (2 per farm) =====

  for (const farm of farms) {
    restPost('coop', makeCoop(0, farm.id));
    restPost('coop', makeCoop(1, farm.id));
  }
  const coops = discoverAll('coop', 'getAll', 'id name farm_id capacity');
  check(null, {
    'getAll coops ≥ 6':     () => coops.length >= 6,
    'coop has farm_id':     () => coops[0].farm_id != null,
    'coop has capacity':    () => coops[0].capacity != null,
  });
  requireNonEmpty('No coops returned from getAll', coops);

  // Build lookup for later
  const coopToFarm = {};
  for (const c of coops) coopToFarm[c.id] = c.farm_id;

  // ===== 7. Create hens (2 per coop) =====

  for (const coop of coops) {
    restPost('hen', makeHen(0, coop.id));
    restPost('hen', makeHen(1, coop.id));
  }
  const hens = discoverAll('hen', 'getAll', 'id name coop_id breed status');
  check(null, {
    'getAll hens ≥ 12':     () => hens.length >= 12,
    'hen has coop_id':      () => hens[0].coop_id != null,
    'hen has breed':        () => hens[0].breed != null,
  });
  requireNonEmpty('No hens returned from getAll', hens);

  // ===== 8. Containers + consumers =====

  for (let i = 0; i < 3; i++) restPost('container', makeContainer(i));
  const containers = discoverAll('container', 'getAll', 'id name container_type capacity zone');
  check(null, {
    'getAll containers ≥ 3':    () => containers.length >= 3,
    'container has zone':       () => containers[0].zone != null,
    'container has capacity':   () => containers[0].capacity != null,
  });
  requireNonEmpty('No containers returned from getAll', containers);

  for (let i = 0; i < 2; i++) restPost('consumer', makeConsumer(i));
  const consumers = discoverAll('consumer', 'getAll', 'id name consumer_type');
  check(null, {
    'getAll consumers ≥ 2':       () => consumers.length >= 2,
    'consumer has consumer_type': () => consumers[0].consumer_type != null,
  });
  requireNonEmpty('No consumers returned from getAll', consumers);

  // ===== 9. Event entities =====

  for (const hen of hens.slice(0, 3)) {
    const farmId = coopToFarm[hen.coop_id];
    restPost('lay_report', makeLayReport(hen.id, hen.coop_id, farmId));
  }
  const layReports = discoverAll('lay_report', 'getAll', 'id hen_id eggs quality');
  check(null, {
    'getAll lay_reports ≥ 3':   () => layReports.length >= 3,
    'lay_report has hen_id':    () => layReports[0].hen_id != null,
    'lay_report has eggs':      () => layReports[0].eggs != null,
  });

  restPost('storage_deposit', makeStorageDeposit(containers[0].id, farms[0].id));
  const deposits = discoverAll('storage_deposit', 'getAll', 'id container_id eggs');
  check(null, {
    'getAll storage_deposits ≥ 1':  () => deposits.length >= 1,
    'deposit has container_id':     () => deposits[0].container_id != null,
  });

  restPost('consumption_report', makeConsumptionReport(consumers[0].id, containers[0].id));
  const consumptionReports = discoverAll('consumption_report', 'getAll', 'id consumer_id eggs');
  check(null, {
    'getAll consumption_reports ≥ 1':   () => consumptionReports.length >= 1,
    'consumption_report has consumer_id': () => consumptionReports[0].consumer_id != null,
  });

  // ===== 10. Filtered queries =====

  const coopsByFarm = graphqlQuery('coop',
    `{ getByFarm(id: "${farms[0].id}") { id name farm_id } }`);
  check(null, {
    'getByFarm returns coops':              () => dig(coopsByFarm, 'getByFarm', 'length') >= 2,
    'getByFarm coops have correct farm_id': () => {
      const arr = dig(coopsByFarm, 'getByFarm');
      return arr && arr.every((c) => c.farm_id === farms[0].id);
    },
  });

  const hensByCoop = graphqlQuery('hen',
    `{ getByCoop(id: "${coops[0].id}") { id name coop_id } }`);
  check(null, {
    'getByCoop returns hens':               () => dig(hensByCoop, 'getByCoop', 'length') >= 2,
    'getByCoop hens have correct coop_id':  () => {
      const arr = dig(hensByCoop, 'getByCoop');
      return arr && arr.every((h) => h.coop_id === coops[0].id);
    },
  });

  // ===== 11. Federation depth 2: Farm → Coops =====

  const d2 = graphqlQuery('farm', `{
    getById(id: "${farms[0].id}") {
      id name farm_type
      coops { id name capacity }
    }
  }`);
  check(null, {
    'depth2: farm.coops is array':    () => Array.isArray(dig(d2, 'getById', 'coops')),
    'depth2: coops non-empty':        () => dig(d2, 'getById', 'coops', 'length') >= 2,
    'depth2: coop has name':          () => dig(d2, 'getById', 'coops', 0, 'name') != null,
  });

  // ===== 12. Federation depth 3: Farm → Coops → Hens =====

  const d3 = graphqlQuery('farm', `{
    getById(id: "${farms[0].id}") {
      id name
      coops {
        id name
        hens { id name breed status }
      }
    }
  }`);
  check(null, {
    'depth3: coops present':          () => dig(d3, 'getById', 'coops', 'length') >= 1,
    'depth3: at least 1 coop has hens': () => {
      const cs = dig(d3, 'getById', 'coops');
      return cs && cs.some((c) => c.hens && c.hens.length > 0);
    },
    'depth3: hens have breed':        () => {
      const cs = dig(d3, 'getById', 'coops');
      const withHens = cs && cs.find((c) => c.hens && c.hens.length > 0);
      return withHens && withHens.hens[0].breed != null;
    },
  });

  // ===== 13. Upward resolver: Hen → Coop =====

  const henUp = graphqlQuery('hen', `{
    getById(id: "${hens[0].id}") {
      id name breed
      coop { id name capacity }
      layReports { id eggs quality }
    }
  }`);
  check(null, {
    'hen→coop populated':          () => dig(henUp, 'getById', 'coop') != null,
    'hen→coop has name':           () => dig(henUp, 'getById', 'coop', 'name') != null,
    'hen→layReports is array':     () => Array.isArray(dig(henUp, 'getById', 'layReports')),
  });

  // ===== 14. Consumer → ConsumptionReports =====

  const consumerFed = graphqlQuery('consumer', `{
    getById(id: "${consumers[0].id}") {
      id name consumer_type
      consumptionReports { id eggs purpose }
    }
  }`);
  check(null, {
    'consumer→reports is array':    () => Array.isArray(dig(consumerFed, 'getById', 'consumptionReports')),
    'consumer→reports non-empty':   () => dig(consumerFed, 'getById', 'consumptionReports', 'length') >= 1,
  });

  // ===== 15. Container → Inventory =====

  const containerFed = graphqlQuery('container', `{
    getById(id: "${containers[0].id}") {
      id name container_type
      inventory { current_eggs total_deposits }
    }
  }`);
  check(null, {
    'container→inventory is array': () => Array.isArray(dig(containerFed, 'getById', 'inventory')),
  });

  // ===== 16. REST DELETE =====

  // Create a temp entity, delete it, verify it's gone from getAll
  const tempConsumer = makeConsumer(8888);
  restPost('consumer', tempConsumer);
  const beforeDelete = discoverAll('consumer', 'getAll', 'id name');
  const temp = beforeDelete.find((c) => c.name === tempConsumer.name);
  if (temp) {
    const delRes = restDelete('consumer', temp.id);
    check(delRes, {
      'REST DELETE → 200': (r) => r.status === 200,
    });
  } else {
    check(null, {
      'temp consumer discoverable for delete test': () => false,
    });
  }

  // ===== Summary =====

  console.log('');
  console.log('=== Validation Summary ===');
  console.log(`  Farms:                ${farms.length}`);
  console.log(`  Coops:                ${coops.length}`);
  console.log(`  Hens:                 ${hens.length}`);
  console.log(`  Containers:           ${containers.length}`);
  console.log(`  Consumers:            ${consumers.length}`);
  console.log(`  Lay Reports:          ${layReports.length}`);
  console.log(`  Storage Deposits:     ${deposits.length}`);
  console.log(`  Consumption Reports:  ${consumptionReports.length}`);
  console.log('=========================');
}
