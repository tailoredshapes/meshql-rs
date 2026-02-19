import { group, sleep } from 'k6';
import { Trend } from 'k6/metrics';
import { getStages, THRESHOLDS } from '../lib/config.js';
import { restPost, graphqlQuery, discoverAll, randomItem } from '../lib/helpers.js';
import { makeFarm, makeCoop, makeHen, makeLayReport, makeContainer } from '../lib/data-generators.js';

const getByIdDuration = new Trend('graphql_getById_duration', true);
const getAllDuration = new Trend('graphql_getAll_duration', true);
const filteredDuration = new Trend('graphql_filtered_duration', true);

export const options = {
  setupTimeout: '120s',
  stages: getStages(),
  thresholds: {
    ...THRESHOLDS,
    graphql_getById_duration: ['p(95)<200'],
    graphql_getAll_duration: ['p(95)<500'],
    graphql_filtered_duration: ['p(95)<300'],
  },
};

export function setup() {
  // Create farms
  for (let i = 0; i < 3; i++) restPost('farm', makeFarm(i));
  const farms = discoverAll('farm', 'getAll', 'id name zone');

  // Create coops (3 per farm)
  for (const farm of farms) {
    for (let i = 0; i < 3; i++) {
      restPost('coop', makeCoop(i, farm.id));
    }
  }
  const coops = discoverAll('coop', 'getAll', 'id name farm_id');

  // Create hens (5 per coop)
  for (const coop of coops) {
    for (let i = 0; i < 5; i++) {
      restPost('hen', makeHen(i, coop.id));
    }
  }
  const hens = discoverAll('hen', 'getAll', 'id name coop_id');

  // Build coop→farm mapping for lay reports
  const coopToFarm = {};
  for (const coop of coops) coopToFarm[coop.id] = coop.farm_id;

  // Create lay reports (3 per hen)
  for (const hen of hens) {
    const farmId = coopToFarm[hen.coop_id];
    for (let i = 0; i < 3; i++) {
      restPost('lay_report', makeLayReport(hen.id, hen.coop_id, farmId));
    }
  }

  // Create containers
  for (let i = 0; i < 4; i++) restPost('container', makeContainer(i));
  const containers = discoverAll('container', 'getAll', 'id name zone');

  return { farms, coops, hens, containers };
}

export default function (data) {
  const { farms, coops, hens, containers } = data;

  group('getById', () => {
    const start = Date.now();
    const farm = randomItem(farms);
    graphqlQuery('farm', `{ getById(id: "${farm.id}") { id name farm_type zone } }`);
    getByIdDuration.add(Date.now() - start);

    const start2 = Date.now();
    const hen = randomItem(hens);
    graphqlQuery('hen', `{ getById(id: "${hen.id}") { id name breed status } }`);
    getByIdDuration.add(Date.now() - start2);

    const start3 = Date.now();
    const container = randomItem(containers);
    graphqlQuery('container', `{ getById(id: "${container.id}") { id name container_type capacity zone } }`);
    getByIdDuration.add(Date.now() - start3);
  });

  group('getAll', () => {
    const start = Date.now();
    graphqlQuery('farm', '{ getAll { id name farm_type zone } }');
    getAllDuration.add(Date.now() - start);

    const start2 = Date.now();
    graphqlQuery('coop', '{ getAll { id name farm_id capacity } }');
    getAllDuration.add(Date.now() - start2);

    const start3 = Date.now();
    graphqlQuery('container', '{ getAll { id name container_type capacity zone } }');
    getAllDuration.add(Date.now() - start3);
  });

  group('filtered', () => {
    const zone = randomItem(farms).zone;

    const start = Date.now();
    graphqlQuery('farm', `{ getByZone(zone: "${zone}") { id name farm_type } }`);
    filteredDuration.add(Date.now() - start);

    const farmId = randomItem(farms).id;
    const start2 = Date.now();
    graphqlQuery('coop', `{ getByFarm(id: "${farmId}") { id name capacity } }`);
    filteredDuration.add(Date.now() - start2);

    const coopId = randomItem(coops).id;
    const start3 = Date.now();
    graphqlQuery('hen', `{ getByCoop(id: "${coopId}") { id name breed status } }`);
    filteredDuration.add(Date.now() - start3);
  });

  sleep(0.5);
}
