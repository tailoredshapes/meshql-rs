// k6 performance test configuration for meshql-rs egg-economy API

export const BASE_URL = __ENV.BASE_URL || 'http://localhost:5088';
export const PROFILE = __ENV.PROFILE || 'load';

export const ENTITIES = {
  farm:                 { rest: '/farm/api',                 graph: '/farm/graph' },
  coop:                 { rest: '/coop/api',                 graph: '/coop/graph' },
  hen:                  { rest: '/hen/api',                  graph: '/hen/graph' },
  container:            { rest: '/container/api',            graph: '/container/graph' },
  consumer:             { rest: '/consumer/api',             graph: '/consumer/graph' },
  lay_report:           { rest: '/lay_report/api',           graph: '/lay_report/graph' },
  storage_deposit:      { rest: '/storage_deposit/api',      graph: '/storage_deposit/graph' },
  storage_withdrawal:   { rest: '/storage_withdrawal/api',   graph: '/storage_withdrawal/graph' },
  container_transfer:   { rest: '/container_transfer/api',   graph: '/container_transfer/graph' },
  consumption_report:   { rest: '/consumption_report/api',   graph: '/consumption_report/graph' },
  container_inventory:  { rest: '/container_inventory/api',  graph: '/container_inventory/graph' },
  hen_productivity:     { rest: '/hen_productivity/api',     graph: '/hen_productivity/graph' },
  farm_output:          { rest: '/farm_output/api',          graph: '/farm_output/graph' },
};

export const STAGES = {
  smoke: [
    { duration: '10s', target: 1 },
  ],
  load: [
    { duration: '10s', target: 5 },
    { duration: '30s', target: 10 },
    { duration: '15s', target: 0 },
  ],
  stress: [
    { duration: '10s', target: 10 },
    { duration: '30s', target: 30 },
    { duration: '30s', target: 50 },
    { duration: '20s', target: 0 },
  ],
};

export const THRESHOLDS = {
  http_req_duration: ['p(95)<500'],
  http_req_failed:   ['rate<0.01'],
};

export function getStages() {
  return STAGES[PROFILE] || STAGES.load;
}
