const FARM_TYPES = ['megafarm', 'local_farm', 'homestead'];
const CONTAINER_TYPES = ['warehouse', 'market_shelf', 'fridge'];
const CONSUMER_TYPES = ['household', 'restaurant', 'bakery'];
const HEN_STATUSES = ['active', 'retired', 'deceased'];
const QUALITIES = ['grade_a', 'grade_b', 'cracked', 'double_yolk'];
const SOURCE_TYPES = ['farm', 'transfer'];
const WITHDRAWAL_REASONS = ['spoilage', 'breakage', 'quality_check'];
const TRANSPORT_METHODS = ['truck', 'van', 'cart'];
const PURPOSES = ['cooking', 'baking', 'resale', 'raw'];
const ZONES = ['north', 'south', 'east', 'west', 'central'];
const BREEDS = ['leghorn', 'rhode_island_red', 'plymouth_rock', 'sussex', 'orpington'];

function pick(arr) {
  return arr[Math.floor(Math.random() * arr.length)];
}

export function makeFarm(i) {
  return {
    name: `farm-${i}-${Date.now()}`,
    farm_type: FARM_TYPES[i % FARM_TYPES.length],
    zone: pick(ZONES),
  };
}

export function makeCoop(i, farmId) {
  return {
    name: `coop-${i}-${Date.now()}`,
    farm_id: farmId,
    capacity: 10 + (i % 40),
    coop_type: `type-${(i % 3) + 1}`,
  };
}

export function makeHen(i, coopId) {
  return {
    name: `hen-${i}-${Date.now()}`,
    coop_id: coopId,
    breed: pick(BREEDS),
    status: pick(HEN_STATUSES),
  };
}

export function makeContainer(i) {
  return {
    name: `container-${i}-${Date.now()}`,
    container_type: CONTAINER_TYPES[i % CONTAINER_TYPES.length],
    capacity: 100 + (i % 400),
    zone: pick(ZONES),
  };
}

export function makeConsumer(i) {
  return {
    name: `consumer-${i}-${Date.now()}`,
    consumer_type: CONSUMER_TYPES[i % CONSUMER_TYPES.length],
    zone: pick(ZONES),
  };
}

export function makeLayReport(henId, coopId, farmId) {
  return {
    hen_id: henId,
    coop_id: coopId,
    farm_id: farmId,
    eggs: Math.floor(Math.random() * 5),
    timestamp: new Date().toISOString(),
    quality: pick(QUALITIES),
  };
}

export function makeStorageDeposit(containerId, sourceId) {
  return {
    container_id: containerId,
    source_type: pick(SOURCE_TYPES),
    source_id: sourceId,
    eggs: 1 + Math.floor(Math.random() * 50),
    timestamp: new Date().toISOString(),
  };
}

export function makeStorageWithdrawal(containerId) {
  return {
    container_id: containerId,
    reason: pick(WITHDRAWAL_REASONS),
    eggs: 1 + Math.floor(Math.random() * 10),
    timestamp: new Date().toISOString(),
  };
}

export function makeContainerTransfer(srcContainerId, dstContainerId) {
  return {
    source_container_id: srcContainerId,
    dest_container_id: dstContainerId,
    eggs: 1 + Math.floor(Math.random() * 30),
    timestamp: new Date().toISOString(),
    transport_method: pick(TRANSPORT_METHODS),
  };
}

export function makeConsumptionReport(consumerId, containerId) {
  return {
    consumer_id: consumerId,
    container_id: containerId,
    eggs: 1 + Math.floor(Math.random() * 12),
    timestamp: new Date().toISOString(),
    purpose: pick(PURPOSES),
  };
}
