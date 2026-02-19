import { group, sleep } from 'k6';
import { getStages, THRESHOLDS } from '../lib/config.js';
import { restPost, graphqlQuery, discoverAll, randomItem } from '../lib/helpers.js';
import {
  makeFarm, makeCoop, makeHen,
  makeContainer, makeConsumer,
  makeLayReport, makeStorageDeposit,
  makeContainerTransfer, makeConsumptionReport,
} from '../lib/data-generators.js';

export const options = {
  setupTimeout: '120s',
  stages: getStages(),
  thresholds: THRESHOLDS,
};

export function setup() {
  // Create actor entities
  for (let i = 0; i < 2; i++) restPost('farm', makeFarm(i));
  const farms = discoverAll('farm', 'getAll', 'id name');

  for (const farm of farms) {
    for (let i = 0; i < 2; i++) restPost('coop', makeCoop(i, farm.id));
  }
  const coops = discoverAll('coop', 'getAll', 'id name farm_id');
  const coopToFarm = {};
  for (const c of coops) coopToFarm[c.id] = c.farm_id;

  for (const coop of coops) {
    for (let i = 0; i < 5; i++) restPost('hen', makeHen(i, coop.id));
  }
  const hens = discoverAll('hen', 'getAll', 'id name coop_id');

  for (let i = 0; i < 4; i++) restPost('container', makeContainer(i));
  const containers = discoverAll('container', 'getAll', 'id name');

  for (let i = 0; i < 3; i++) restPost('consumer', makeConsumer(i));
  const consumers = discoverAll('consumer', 'getAll', 'id name');

  return { farms, coops, hens, containers, consumers, coopToFarm };
}

export default function (data) {
  const { farms, coops, hens, containers, consumers, coopToFarm } = data;

  group('Write events', () => {
    // Lay report
    const hen = randomItem(hens);
    const farmId = coopToFarm[hen.coop_id];
    restPost('lay_report', makeLayReport(hen.id, hen.coop_id, farmId));

    // Storage deposit
    const container = randomItem(containers);
    restPost('storage_deposit', makeStorageDeposit(container.id, randomItem(farms).id));

    // Container transfer (between two different containers)
    const src = containers[0];
    const dst = containers.length > 1 ? containers[1] : containers[0];
    restPost('container_transfer', makeContainerTransfer(src.id, dst.id));

    // Consumption report
    const consumer = randomItem(consumers);
    restPost('consumption_report', makeConsumptionReport(consumer.id, randomItem(containers).id));
  });

  group('Read projections', () => {
    graphqlQuery('farm_output', '{ getAll { farm_id eggs_today eggs_week active_hens } }');
    graphqlQuery('hen_productivity', '{ getAll { hen_id total_eggs avg_per_week quality_rate } }');
    graphqlQuery('container_inventory', '{ getAll { container_id current_eggs total_deposits total_withdrawals } }');
  });

  group('Read relationships', () => {
    const farm = randomItem(farms);
    graphqlQuery('farm', `{
      getById(id: "${farm.id}") {
        id name farm_type
        coops {
          id name
          hens { id name breed }
        }
        farmOutput { eggs_today active_hens }
      }
    }`);

    const consumer = randomItem(consumers);
    graphqlQuery('consumer', `{
      getById(id: "${consumer.id}") {
        id name consumer_type
        consumptionReports {
          id eggs purpose timestamp
        }
      }
    }`);
  });

  sleep(0.5);
}
