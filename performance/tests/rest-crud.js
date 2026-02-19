import { group, sleep } from 'k6';
import { getStages, THRESHOLDS } from '../lib/config.js';
import { restPost, restGet, restPut, restDelete, discoverAll, randomItem } from '../lib/helpers.js';
import { makeFarm, makeCoop, makeHen, makeContainer, makeConsumer } from '../lib/data-generators.js';

export const options = {
  setupTimeout: '120s',
  stages: getStages(),
  thresholds: THRESHOLDS,
};

export function setup() {
  // Create farms
  for (let i = 0; i < 3; i++) restPost('farm', makeFarm(i));
  const farms = discoverAll('farm', 'getAll', 'id name');

  // Create coops (2 per farm)
  for (let i = 0; i < farms.length; i++) {
    restPost('coop', makeCoop(i * 2, farms[i].id));
    restPost('coop', makeCoop(i * 2 + 1, farms[i].id));
  }
  const coops = discoverAll('coop', 'getAll', 'id name');

  // Create hens (3 per coop)
  for (let i = 0; i < coops.length; i++) {
    for (let j = 0; j < 3; j++) {
      restPost('hen', makeHen(i * 3 + j, coops[i].id));
    }
  }
  const hens = discoverAll('hen', 'getAll', 'id name');

  // Create containers
  for (let i = 0; i < 3; i++) restPost('container', makeContainer(i));
  const containers = discoverAll('container', 'getAll', 'id name');

  // Create consumers
  for (let i = 0; i < 3; i++) restPost('consumer', makeConsumer(i));
  const consumers = discoverAll('consumer', 'getAll', 'id name');

  return { farms, coops, hens, containers, consumers };
}

export default function (data) {
  const { farms, coops, containers, consumers } = data;

  group('Create', () => {
    const i = Math.floor(Math.random() * 1000);
    restPost('farm', makeFarm(i));
    restPost('coop', makeCoop(i, randomItem(farms).id));
    restPost('container', makeContainer(i));
    restPost('consumer', makeConsumer(i));
  });

  group('Read', () => {
    restGet('farm', randomItem(farms).id);
    restGet('coop', randomItem(coops).id);
    restGet('container', randomItem(containers).id);
    restGet('consumer', randomItem(consumers).id);
  });

  group('Update', () => {
    const farm = randomItem(farms);
    restPut('farm', farm.id, { name: `updated-${Date.now()}`, farm_type: 'homestead', zone: 'central' });

    const container = randomItem(containers);
    restPut('container', container.id, { name: `updated-${Date.now()}`, container_type: 'warehouse', capacity: 200, zone: 'north' });
  });

  group('Delete+Recreate', () => {
    // Create a temporary entity, then delete it
    restPost('consumer', makeConsumer(9999));
    const tempConsumers = discoverAll('consumer', 'getAll', 'id name');
    const temp = tempConsumers.find((c) => c.name.startsWith('consumer-9999-'));
    if (temp) {
      restDelete('consumer', temp.id);
      restPost('consumer', makeConsumer(9998));
    }
  });

  sleep(0.5);
}
