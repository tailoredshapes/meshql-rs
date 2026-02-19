import { group, sleep } from 'k6';
import { Trend } from 'k6/metrics';
import { getStages, THRESHOLDS } from '../lib/config.js';
import { restPost, graphqlQuery, discoverAll, randomItem } from '../lib/helpers.js';
import {
  makeFarm, makeCoop, makeHen, makeLayReport,
  makeContainer, makeStorageDeposit,
  makeConsumer, makeConsumptionReport,
} from '../lib/data-generators.js';

const depth2Duration = new Trend('federation_depth2_duration', true);
const depth3Duration = new Trend('federation_depth3_duration', true);
const depth4Duration = new Trend('federation_depth4_duration', true);

export const options = {
  setupTimeout: '120s',
  stages: getStages(),
  thresholds: {
    ...THRESHOLDS,
    federation_depth2_duration: ['p(95)<500'],
    federation_depth3_duration: ['p(95)<1000'],
    federation_depth4_duration: ['p(95)<2000'],
  },
};

export function setup() {
  // Create farms
  for (let i = 0; i < 2; i++) restPost('farm', makeFarm(i));
  const farms = discoverAll('farm', 'getAll', 'id name');

  // Create coops (3 per farm)
  for (const farm of farms) {
    for (let i = 0; i < 3; i++) restPost('coop', makeCoop(i, farm.id));
  }
  const coops = discoverAll('coop', 'getAll', 'id name farm_id');
  const coopToFarm = {};
  for (const c of coops) coopToFarm[c.id] = c.farm_id;

  // Create hens (4 per coop)
  for (const coop of coops) {
    for (let i = 0; i < 4; i++) restPost('hen', makeHen(i, coop.id));
  }
  const hens = discoverAll('hen', 'getAll', 'id name coop_id');

  // Create lay reports (2 per hen)
  for (const hen of hens) {
    const farmId = coopToFarm[hen.coop_id];
    for (let i = 0; i < 2; i++) {
      restPost('lay_report', makeLayReport(hen.id, hen.coop_id, farmId));
    }
  }
  const layReports = discoverAll('lay_report', 'getAll', 'id hen_id');

  // Create containers + deposits
  for (let i = 0; i < 3; i++) restPost('container', makeContainer(i));
  const containers = discoverAll('container', 'getAll', 'id name');
  for (const container of containers) {
    restPost('storage_deposit', makeStorageDeposit(container.id, randomItem(farms).id));
  }

  // Create consumers + consumption reports
  for (let i = 0; i < 2; i++) restPost('consumer', makeConsumer(i));
  const consumers = discoverAll('consumer', 'getAll', 'id name');
  for (const consumer of consumers) {
    restPost('consumption_report', makeConsumptionReport(consumer.id, randomItem(containers).id));
  }

  return { farms, coops, hens, layReports, containers, consumers };
}

export default function (data) {
  const { farms, coops, hens, containers } = data;

  group('Depth 2', () => {
    const farm = randomItem(farms);
    const start = Date.now();
    graphqlQuery('farm', `{
      getById(id: "${farm.id}") {
        id name farm_type
        coops { id name capacity }
      }
    }`);
    depth2Duration.add(Date.now() - start);
  });

  group('Depth 3', () => {
    const farm = randomItem(farms);
    const start = Date.now();
    graphqlQuery('farm', `{
      getById(id: "${farm.id}") {
        id name farm_type
        coops {
          id name capacity
          hens { id name breed status }
        }
      }
    }`);
    depth3Duration.add(Date.now() - start);
  });

  group('Depth 3 parallel', () => {
    // Farm with both coops+hens AND farmOutput resolved in parallel
    const farm = randomItem(farms);
    const start = Date.now();
    graphqlQuery('farm', `{
      getById(id: "${farm.id}") {
        id name farm_type
        coops {
          id name capacity
          hens { id name breed status }
        }
        farmOutput { eggs_today eggs_week active_hens }
      }
    }`);
    depth4Duration.add(Date.now() - start);
  });

  group('Container+inventory', () => {
    const container = randomItem(containers);
    graphqlQuery('container', `{
      getById(id: "${container.id}") {
        id name container_type capacity
        inventory { current_eggs total_deposits total_withdrawals }
      }
    }`);
  });

  group('Hen+resolvers', () => {
    // Hen with coop (up) + layReports (down) + productivity resolved
    const hen = randomItem(hens);
    graphqlQuery('hen', `{
      getById(id: "${hen.id}") {
        id name breed
        coop { id name capacity }
        layReports { id eggs quality timestamp }
        productivity { total_eggs avg_per_week quality_rate }
      }
    }`);
  });

  sleep(0.5);
}
