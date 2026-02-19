-- Egg Economy Salesforce Legacy Database
-- Models a realistic Salesforce org replicated to PostgreSQL (e.g., via Heroku Connect or ETL).
--
-- Salesforce conventions modeled here:
--   - Custom objects: lowercase with __c suffix (PostgreSQL lowercases unquoted identifiers)
--   - Custom fields: lowercase with __c suffix
--   - Standard fields: id (18-char), name, createddate, createdbyid, lastmodifieddate,
--     lastmodifiedbyid, systemmodstamp, isdeleted, ownerid
--   - Lookup fields: named after parent object (e.g., farm__c on coop__c)
--   - Master-Detail children: no ownerid (inherited from master)
--   - Auto-number Name fields on transaction objects (e.g., LR-00001)
--   - Picklist values in Title Case
--   - User IDs use the 005 key prefix

-- ============================================================
-- FARM (Farm__c) — top-level custom object, has OwnerId
-- ============================================================
CREATE TABLE farm__c (
    id                  VARCHAR(18) PRIMARY KEY,       -- Salesforce 18-char ID (key prefix a0B)
    name                VARCHAR(80) NOT NULL,          -- Text Name field
    farm_type__c        VARCHAR(40) NOT NULL,          -- Picklist: Megafarm, Local Farm, Homestead
    zone__c             VARCHAR(40) NOT NULL,
    owner_name__c       VARCHAR(80),                   -- Custom text field (business owner, not SF record owner)
    ownerid             VARCHAR(18) NOT NULL,           -- Standard: Salesforce record owner (User ID)
    createdbyid         VARCHAR(18) NOT NULL,           -- Standard: User who created the record
    createddate         TIMESTAMP NOT NULL,             -- Standard: creation timestamp
    lastmodifiedbyid    VARCHAR(18) NOT NULL,           -- Standard: User who last modified
    lastmodifieddate    TIMESTAMP NOT NULL,             -- Standard: last modification timestamp
    systemmodstamp      TIMESTAMP NOT NULL,             -- Standard: internal sync timestamp (>= LastModifiedDate)
    isdeleted           BOOLEAN DEFAULT FALSE           -- Standard: soft delete flag
);

ALTER TABLE farm__c REPLICA IDENTITY FULL;

-- ============================================================
-- COOP (Coop__c) — Master-Detail to Farm (no OwnerId, inherits from Farm)
-- ============================================================
CREATE TABLE coop__c (
    id                  VARCHAR(18) PRIMARY KEY,       -- Key prefix a0C
    name                VARCHAR(80) NOT NULL,          -- Text Name field
    farm__c             VARCHAR(18) NOT NULL REFERENCES farm__c(id),    -- Master-Detail to Farm
    capacity__c         INTEGER NOT NULL,
    coop_type__c        VARCHAR(40) NOT NULL,          -- Picklist: Free Range, Cage, Barn
    -- No ownerid: Master-Detail child inherits owner from Farm
    createdbyid         VARCHAR(18) NOT NULL,
    createddate         TIMESTAMP NOT NULL,
    lastmodifiedbyid    VARCHAR(18) NOT NULL,
    lastmodifieddate    TIMESTAMP NOT NULL,
    systemmodstamp      TIMESTAMP NOT NULL,
    isdeleted           BOOLEAN DEFAULT FALSE
);

ALTER TABLE coop__c REPLICA IDENTITY FULL;

-- ============================================================
-- HEN (Hen__c) — Master-Detail to Coop (no OwnerId, inherits from Coop→Farm)
-- ============================================================
CREATE TABLE hen__c (
    id                  VARCHAR(18) PRIMARY KEY,       -- Key prefix a0D
    name                VARCHAR(80) NOT NULL,          -- Text Name field (hen's name)
    coop__c             VARCHAR(18) NOT NULL REFERENCES coop__c(id),    -- Master-Detail to Coop
    breed__c            VARCHAR(40) NOT NULL,          -- Picklist: Rhode Island Red, Leghorn, etc.
    date_of_birth__c    DATE,
    status__c           VARCHAR(20) DEFAULT 'Active',  -- Picklist: Active, Retired, Deceased
    -- No ownerid: Master-Detail child inherits through Coop→Farm
    createdbyid         VARCHAR(18) NOT NULL,
    createddate         TIMESTAMP NOT NULL,
    lastmodifiedbyid    VARCHAR(18) NOT NULL,
    lastmodifieddate    TIMESTAMP NOT NULL,
    systemmodstamp      TIMESTAMP NOT NULL,
    isdeleted           BOOLEAN DEFAULT FALSE
);

ALTER TABLE hen__c REPLICA IDENTITY FULL;

-- ============================================================
-- STORAGE CONTAINER (Storage_Container__c) — top-level, has OwnerId
-- ============================================================
CREATE TABLE storage_container__c (
    id                  VARCHAR(18) PRIMARY KEY,       -- Key prefix a0E
    name                VARCHAR(80) NOT NULL,          -- Text Name field
    container_type__c   VARCHAR(40) NOT NULL,          -- Picklist: Warehouse, Shelf, Fridge
    capacity__c         INTEGER NOT NULL,
    zone__c             VARCHAR(40) NOT NULL,
    ownerid             VARCHAR(18) NOT NULL,
    createdbyid         VARCHAR(18) NOT NULL,
    createddate         TIMESTAMP NOT NULL,
    lastmodifiedbyid    VARCHAR(18) NOT NULL,
    lastmodifieddate    TIMESTAMP NOT NULL,
    systemmodstamp      TIMESTAMP NOT NULL,
    isdeleted           BOOLEAN DEFAULT FALSE
);

ALTER TABLE storage_container__c REPLICA IDENTITY FULL;

-- ============================================================
-- CONSUMER ACCOUNT (Consumer_Account__c) — top-level, has OwnerId
-- ============================================================
CREATE TABLE consumer_account__c (
    id                  VARCHAR(18) PRIMARY KEY,       -- Key prefix a0F
    name                VARCHAR(80) NOT NULL,          -- Text Name field
    consumer_type__c    VARCHAR(40) NOT NULL,          -- Picklist: Household, Restaurant, Bakery
    zone__c             VARCHAR(40) NOT NULL,
    weekly_demand__c    INTEGER DEFAULT 0,
    ownerid             VARCHAR(18) NOT NULL,
    createdbyid         VARCHAR(18) NOT NULL,
    createddate         TIMESTAMP NOT NULL,
    lastmodifiedbyid    VARCHAR(18) NOT NULL,
    lastmodifieddate    TIMESTAMP NOT NULL,
    systemmodstamp      TIMESTAMP NOT NULL,
    isdeleted           BOOLEAN DEFAULT FALSE
);

ALTER TABLE consumer_account__c REPLICA IDENTITY FULL;

-- ============================================================
-- LAY REPORT (Lay_Report__c) — Lookup to Hen + Farm, has OwnerId
-- Auto-number Name: LR-{00000}
-- ============================================================
CREATE TABLE lay_report__c (
    id                  VARCHAR(18) PRIMARY KEY,       -- Key prefix a0G
    name                VARCHAR(80) NOT NULL,          -- Auto Number: LR-00001, LR-00002, etc.
    hen__c              VARCHAR(18) NOT NULL REFERENCES hen__c(id),     -- Lookup to Hen
    farm__c             VARCHAR(18) NOT NULL REFERENCES farm__c(id),    -- Lookup to Farm
    egg_count__c        INTEGER NOT NULL,
    quality__c          VARCHAR(20) DEFAULT 'Grade A',  -- Picklist: Grade A, Grade B, Cracked, Double Yolk
    ownerid             VARCHAR(18) NOT NULL,
    createdbyid         VARCHAR(18) NOT NULL,
    createddate         TIMESTAMP NOT NULL,
    lastmodifiedbyid    VARCHAR(18) NOT NULL,
    lastmodifieddate    TIMESTAMP NOT NULL,
    systemmodstamp      TIMESTAMP NOT NULL,
    isdeleted           BOOLEAN DEFAULT FALSE
);

ALTER TABLE lay_report__c REPLICA IDENTITY FULL;

-- ============================================================
-- STORAGE DEPOSIT (Storage_Deposit__c) — Lookup to Container, has OwnerId
-- Auto-number Name: SD-{00000}
-- ============================================================
CREATE TABLE storage_deposit__c (
    id                  VARCHAR(18) PRIMARY KEY,       -- Key prefix a0H
    name                VARCHAR(80) NOT NULL,          -- Auto Number: SD-00001, SD-00002, etc.
    container__c        VARCHAR(18) NOT NULL REFERENCES storage_container__c(id),  -- Lookup
    source_type__c      VARCHAR(20) NOT NULL,          -- Picklist: Farm, Transfer
    egg_count__c        INTEGER NOT NULL,
    ownerid             VARCHAR(18) NOT NULL,
    createdbyid         VARCHAR(18) NOT NULL,
    createddate         TIMESTAMP NOT NULL,
    lastmodifiedbyid    VARCHAR(18) NOT NULL,
    lastmodifieddate    TIMESTAMP NOT NULL,
    systemmodstamp      TIMESTAMP NOT NULL,
    isdeleted           BOOLEAN DEFAULT FALSE
);

ALTER TABLE storage_deposit__c REPLICA IDENTITY FULL;

-- ============================================================
-- STORAGE WITHDRAWAL (Storage_Withdrawal__c) — Lookup to Container, has OwnerId
-- Auto-number Name: SW-{00000}
-- ============================================================
CREATE TABLE storage_withdrawal__c (
    id                  VARCHAR(18) PRIMARY KEY,       -- Key prefix a0I
    name                VARCHAR(80) NOT NULL,          -- Auto Number: SW-00001, SW-00002, etc.
    container__c        VARCHAR(18) NOT NULL REFERENCES storage_container__c(id),  -- Lookup
    egg_count__c        INTEGER NOT NULL,
    reason__c           VARCHAR(20) DEFAULT 'Spoilage',  -- Picklist: Spoilage, Breakage, Quality Check
    ownerid             VARCHAR(18) NOT NULL,
    createdbyid         VARCHAR(18) NOT NULL,
    createddate         TIMESTAMP NOT NULL,
    lastmodifiedbyid    VARCHAR(18) NOT NULL,
    lastmodifieddate    TIMESTAMP NOT NULL,
    systemmodstamp      TIMESTAMP NOT NULL,
    isdeleted           BOOLEAN DEFAULT FALSE
);

ALTER TABLE storage_withdrawal__c REPLICA IDENTITY FULL;

-- ============================================================
-- CONTAINER TRANSFER (Container_Transfer__c) — Lookup to Container x2, has OwnerId
-- Auto-number Name: CT-{00000}
-- ============================================================
CREATE TABLE container_transfer__c (
    id                  VARCHAR(18) PRIMARY KEY,       -- Key prefix a0J
    name                VARCHAR(80) NOT NULL,          -- Auto Number: CT-00001, CT-00002, etc.
    source_container__c VARCHAR(18) NOT NULL REFERENCES storage_container__c(id),  -- Lookup
    dest_container__c   VARCHAR(18) NOT NULL REFERENCES storage_container__c(id),  -- Lookup
    egg_count__c        INTEGER NOT NULL,
    transport_method__c VARCHAR(20) DEFAULT 'Truck',    -- Picklist: Truck, Van, Cart
    ownerid             VARCHAR(18) NOT NULL,
    createdbyid         VARCHAR(18) NOT NULL,
    createddate         TIMESTAMP NOT NULL,
    lastmodifiedbyid    VARCHAR(18) NOT NULL,
    lastmodifieddate    TIMESTAMP NOT NULL,
    systemmodstamp      TIMESTAMP NOT NULL,
    isdeleted           BOOLEAN DEFAULT FALSE
);

ALTER TABLE container_transfer__c REPLICA IDENTITY FULL;

-- ============================================================
-- CONSUMPTION REPORT (Consumption_Report__c) — Lookup to Consumer + Container, has OwnerId
-- Auto-number Name: CR-{00000}
-- ============================================================
CREATE TABLE consumption_report__c (
    id                  VARCHAR(18) PRIMARY KEY,       -- Key prefix a0K
    name                VARCHAR(80) NOT NULL,          -- Auto Number: CR-00001, CR-00002, etc.
    consumer__c         VARCHAR(18) NOT NULL REFERENCES consumer_account__c(id),   -- Lookup
    container__c        VARCHAR(18) NOT NULL REFERENCES storage_container__c(id),  -- Lookup
    egg_count__c        INTEGER NOT NULL,
    purpose__c          VARCHAR(20) DEFAULT 'Cooking',  -- Picklist: Cooking, Baking, Resale, Raw
    ownerid             VARCHAR(18) NOT NULL,
    createdbyid         VARCHAR(18) NOT NULL,
    createddate         TIMESTAMP NOT NULL,
    lastmodifiedbyid    VARCHAR(18) NOT NULL,
    lastmodifieddate    TIMESTAMP NOT NULL,
    systemmodstamp      TIMESTAMP NOT NULL,
    isdeleted           BOOLEAN DEFAULT FALSE
);

ALTER TABLE consumption_report__c REPLICA IDENTITY FULL;

-- ============================================================
-- SEED DATA
-- ============================================================

-- Salesforce User IDs (005 key prefix)
-- 0055f000001USR01 = John Henderson (System Administrator)
-- 0055f000001USR02 = Anna Williams (Standard User)
-- 0055f000001USR03 = Klaus Weber (Standard User)

-- Farms (key prefix a0B, Text Name)
INSERT INTO farm__c (id, name, farm_type__c, zone__c, owner_name__c, ownerid, createdbyid, createddate, lastmodifiedbyid, lastmodifieddate, systemmodstamp) VALUES
('a0B5f000001ABC01', 'Sunny Valley Megafarm',   'Megafarm',    'north', 'John Henderson', '0055f000001USR01', '0055f000001USR01', '2015-03-01T08:00:00', '0055f000001USR01', '2024-11-15T14:22:00', '2024-11-15T14:22:01'),
('a0B5f000001ABC02', 'Green Pastures Farm',      'Local Farm',  'south', 'Anna Williams',  '0055f000001USR02', '0055f000001USR02', '2018-06-15T09:30:00', '0055f000001USR02', '2025-08-20T11:05:00', '2025-08-20T11:05:00'),
('a0B5f000001ABC03', 'Mountain View Homestead',  'Homestead',   'east',  'Klaus Weber',    '0055f000001USR03', '0055f000001USR01', '2020-01-01T07:00:00', '0055f000001USR03', '2020-01-01T07:00:00', '2020-01-01T07:00:01');

-- Coops (key prefix a0C, Master-Detail to Farm — no ownerid)
INSERT INTO coop__c (id, name, farm__c, capacity__c, coop_type__c, createdbyid, createddate, lastmodifiedbyid, lastmodifieddate, systemmodstamp) VALUES
('a0C5f000002DEF01', 'Alpha Coop',      'a0B5f000001ABC01', 500, 'Cage',       '0055f000001USR01', '2015-04-01T10:00:00', '0055f000001USR01', '2023-03-10T09:15:00', '2023-03-10T09:15:01'),
('a0C5f000002DEF02', 'Beta Coop',       'a0B5f000001ABC01', 500, 'Cage',       '0055f000001USR01', '2015-04-15T10:00:00', '0055f000001USR01', '2015-04-15T10:00:00', '2015-04-15T10:00:01'),
('a0C5f000002DEF03', 'Free Range Coop', 'a0B5f000001ABC02', 100, 'Free Range', '0055f000001USR02', '2018-07-01T11:00:00', '0055f000001USR02', '2018-07-01T11:00:00', '2018-07-01T11:00:00'),
('a0C5f000002DEF04', 'Mountain Coop',   'a0B5f000001ABC03', 25,  'Barn',       '0055f000001USR03', '2020-02-01T08:00:00', '0055f000001USR03', '2020-02-01T08:00:00', '2020-02-01T08:00:01');

-- Hens (key prefix a0D, Master-Detail to Coop — no ownerid)
INSERT INTO hen__c (id, name, coop__c, breed__c, date_of_birth__c, status__c, createdbyid, createddate, lastmodifiedbyid, lastmodifieddate, systemmodstamp) VALUES
('a0D5f000003GHI01', 'Greta',  'a0C5f000002DEF01', 'Rhode Island Red', '2023-01-15', 'Active',  '0055f000001USR01', '2023-01-15T06:00:00', '0055f000001USR01', '2023-01-15T06:00:00', '2023-01-15T06:00:01'),
('a0D5f000003GHI02', 'Liesel', 'a0C5f000002DEF01', 'Leghorn',          '2023-02-20', 'Active',  '0055f000001USR01', '2023-02-20T06:00:00', '0055f000001USR01', '2023-02-20T06:00:00', '2023-02-20T06:00:01'),
('a0D5f000003GHI03', 'Berta',  'a0C5f000002DEF02', 'Plymouth Rock',    '2022-08-01', 'Active',  '0055f000001USR01', '2022-08-01T06:00:00', '0055f000001USR01', '2022-08-01T06:00:00', '2022-08-01T06:00:00'),
('a0D5f000003GHI04', 'Frieda', 'a0C5f000002DEF03', 'Sussex',           '2023-06-01', 'Active',  '0055f000001USR02', '2023-06-01T06:00:00', '0055f000001USR02', '2023-06-01T06:00:00', '2023-06-01T06:00:00'),
('a0D5f000003GHI05', 'Klara',  'a0C5f000002DEF03', 'Rhode Island Red', '2023-09-15', 'Active',  '0055f000001USR02', '2023-09-15T06:00:00', '0055f000001USR02', '2023-09-15T06:00:00', '2023-09-15T06:00:01'),
('a0D5f000003GHI06', 'Rosa',   'a0C5f000002DEF04', 'Orpington',        '2024-01-01', 'Active',  '0055f000001USR03', '2024-01-01T06:00:00', '0055f000001USR03', '2024-01-01T06:00:00', '2024-01-01T06:00:00'),
('a0D5f000003GHI07', 'Elsa',   'a0C5f000002DEF01', 'Leghorn',          '2022-03-15', 'Retired', '0055f000001USR01', '2022-03-15T06:00:00', '0055f000001USR01', '2025-12-01T10:30:00', '2025-12-01T10:30:01');

-- Containers (key prefix a0E)
INSERT INTO storage_container__c (id, name, container_type__c, capacity__c, zone__c, ownerid, createdbyid, createddate, lastmodifiedbyid, lastmodifieddate, systemmodstamp) VALUES
('a0E5f000004JKL01', 'North Warehouse',    'Warehouse', 10000, 'north', '0055f000001USR01', '0055f000001USR01', '2015-01-01T08:00:00', '0055f000001USR01', '2015-01-01T08:00:00', '2015-01-01T08:00:01'),
('a0E5f000004JKL02', 'South Warehouse',    'Warehouse', 5000,  'south', '0055f000001USR02', '0055f000001USR02', '2018-01-01T08:00:00', '0055f000001USR02', '2018-01-01T08:00:00', '2018-01-01T08:00:00'),
('a0E5f000004JKL03', 'Market Shelf North', 'Shelf',     200,   'north', '0055f000001USR01', '0055f000001USR01', '2019-06-01T08:00:00', '0055f000001USR01', '2019-06-01T08:00:00', '2019-06-01T08:00:01'),
('a0E5f000004JKL04', 'East Fridge',        'Fridge',    50,    'east',  '0055f000001USR03', '0055f000001USR03', '2020-03-01T08:00:00', '0055f000001USR03', '2020-03-01T08:00:00', '2020-03-01T08:00:00');

-- Consumers (key prefix a0F)
INSERT INTO consumer_account__c (id, name, consumer_type__c, zone__c, weekly_demand__c, ownerid, createdbyid, createddate, lastmodifiedbyid, lastmodifieddate, systemmodstamp) VALUES
('a0F5f000005MNO01', 'Brown Family',     'Household',  'north', 12,  '0055f000001USR01', '0055f000001USR01', '2020-01-01T08:00:00', '0055f000001USR01', '2020-01-01T08:00:00', '2020-01-01T08:00:01'),
('a0F5f000005MNO02', 'Eagle Restaurant', 'Restaurant', 'south', 200, '0055f000001USR02', '0055f000001USR02', '2019-06-01T08:00:00', '0055f000001USR02', '2025-01-10T16:45:00', '2025-01-10T16:45:01'),
('a0F5f000005MNO03', 'Meier Bakery',     'Bakery',     'north', 500, '0055f000001USR01', '0055f000001USR01', '2018-03-01T08:00:00', '0055f000001USR02', '2024-06-01T09:00:00', '2024-06-01T09:00:00');

-- Lay Reports (key prefix a0G, Auto Number name)
INSERT INTO lay_report__c (id, name, hen__c, farm__c, egg_count__c, quality__c, ownerid, createdbyid, createddate, lastmodifiedbyid, lastmodifieddate, systemmodstamp) VALUES
('a0G5f000006PQR01', 'LR-00001', 'a0D5f000003GHI01', 'a0B5f000001ABC01', 3, 'Grade A',     '0055f000001USR01', '0055f000001USR01', '2026-02-01T06:30:00', '0055f000001USR01', '2026-02-01T06:30:00', '2026-02-01T06:30:00'),
('a0G5f000006PQR02', 'LR-00002', 'a0D5f000003GHI02', 'a0B5f000001ABC01', 2, 'Grade A',     '0055f000001USR01', '0055f000001USR01', '2026-02-01T06:45:00', '0055f000001USR01', '2026-02-01T06:45:00', '2026-02-01T06:45:01'),
('a0G5f000006PQR03', 'LR-00003', 'a0D5f000003GHI03', 'a0B5f000001ABC01', 4, 'Double Yolk', '0055f000001USR01', '0055f000001USR01', '2026-02-01T07:00:00', '0055f000001USR01', '2026-02-01T07:00:00', '2026-02-01T07:00:00'),
('a0G5f000006PQR04', 'LR-00004', 'a0D5f000003GHI04', 'a0B5f000001ABC02', 2, 'Grade A',     '0055f000001USR02', '0055f000001USR02', '2026-02-01T07:15:00', '0055f000001USR02', '2026-02-01T07:15:00', '2026-02-01T07:15:00'),
('a0G5f000006PQR05', 'LR-00005', 'a0D5f000003GHI05', 'a0B5f000001ABC02', 1, 'Grade B',     '0055f000001USR02', '0055f000001USR02', '2026-02-01T07:30:00', '0055f000001USR02', '2026-02-01T07:30:00', '2026-02-01T07:30:01'),
('a0G5f000006PQR06', 'LR-00006', 'a0D5f000003GHI06', 'a0B5f000001ABC03', 1, 'Grade A',     '0055f000001USR03', '0055f000001USR03', '2026-02-01T08:00:00', '0055f000001USR03', '2026-02-01T08:00:00', '2026-02-01T08:00:00'),
('a0G5f000006PQR07', 'LR-00007', 'a0D5f000003GHI01', 'a0B5f000001ABC01', 4, 'Grade A',     '0055f000001USR01', '0055f000001USR01', '2026-02-02T06:30:00', '0055f000001USR01', '2026-02-02T06:30:00', '2026-02-02T06:30:00'),
('a0G5f000006PQR08', 'LR-00008', 'a0D5f000003GHI04', 'a0B5f000001ABC02', 3, 'Grade A',     '0055f000001USR02', '0055f000001USR02', '2026-02-02T07:15:00', '0055f000001USR02', '2026-02-02T07:15:00', '2026-02-02T07:15:01');

-- Storage Deposits (key prefix a0H, Auto Number name)
INSERT INTO storage_deposit__c (id, name, container__c, source_type__c, egg_count__c, ownerid, createdbyid, createddate, lastmodifiedbyid, lastmodifieddate, systemmodstamp) VALUES
('a0H5f000007STU01', 'SD-00001', 'a0E5f000004JKL01', 'Farm', 9, '0055f000001USR01', '0055f000001USR01', '2026-02-01T10:00:00', '0055f000001USR01', '2026-02-01T10:00:00', '2026-02-01T10:00:01'),
('a0H5f000007STU02', 'SD-00002', 'a0E5f000004JKL02', 'Farm', 3, '0055f000001USR02', '0055f000001USR02', '2026-02-01T11:00:00', '0055f000001USR02', '2026-02-01T11:00:00', '2026-02-01T11:00:00'),
('a0H5f000007STU03', 'SD-00003', 'a0E5f000004JKL04', 'Farm', 1, '0055f000001USR03', '0055f000001USR03', '2026-02-01T12:00:00', '0055f000001USR03', '2026-02-01T12:00:00', '2026-02-01T12:00:00'),
('a0H5f000007STU04', 'SD-00004', 'a0E5f000004JKL01', 'Farm', 7, '0055f000001USR01', '0055f000001USR01', '2026-02-02T10:00:00', '0055f000001USR01', '2026-02-02T10:00:00', '2026-02-02T10:00:01');

-- Storage Withdrawals (key prefix a0I, Auto Number name)
INSERT INTO storage_withdrawal__c (id, name, container__c, egg_count__c, reason__c, ownerid, createdbyid, createddate, lastmodifiedbyid, lastmodifieddate, systemmodstamp) VALUES
('a0I5f000008VWX01', 'SW-00001', 'a0E5f000004JKL01', 2, 'Spoilage', '0055f000001USR01', '0055f000001USR01', '2026-02-01T14:00:00', '0055f000001USR01', '2026-02-01T14:00:00', '2026-02-01T14:00:00'),
('a0I5f000008VWX02', 'SW-00002', 'a0E5f000004JKL02', 1, 'Breakage', '0055f000001USR02', '0055f000001USR02', '2026-02-01T15:00:00', '0055f000001USR02', '2026-02-01T15:00:00', '2026-02-01T15:00:01');

-- Container Transfers (key prefix a0J, Auto Number name)
INSERT INTO container_transfer__c (id, name, source_container__c, dest_container__c, egg_count__c, transport_method__c, ownerid, createdbyid, createddate, lastmodifiedbyid, lastmodifieddate, systemmodstamp) VALUES
('a0J5f000009YZA01', 'CT-00001', 'a0E5f000004JKL01', 'a0E5f000004JKL03', 24, 'Van',  '0055f000001USR01', '0055f000001USR01', '2026-02-01T16:00:00', '0055f000001USR01', '2026-02-01T16:00:00', '2026-02-01T16:00:00'),
('a0J5f000009YZA02', 'CT-00002', 'a0E5f000004JKL02', 'a0E5f000004JKL04', 6,  'Cart', '0055f000001USR02', '0055f000001USR02', '2026-02-02T09:00:00', '0055f000001USR02', '2026-02-02T09:00:00', '2026-02-02T09:00:01');

-- Consumption Reports (key prefix a0K, Auto Number name)
INSERT INTO consumption_report__c (id, name, consumer__c, container__c, egg_count__c, purpose__c, ownerid, createdbyid, createddate, lastmodifiedbyid, lastmodifieddate, systemmodstamp) VALUES
('a0K5f00000ABCD01', 'CR-00001', 'a0F5f000005MNO01', 'a0E5f000004JKL03', 12,  'Cooking', '0055f000001USR01', '0055f000001USR01', '2026-02-01T18:00:00', '0055f000001USR01', '2026-02-01T18:00:00', '2026-02-01T18:00:01'),
('a0K5f00000ABCD02', 'CR-00002', 'a0F5f000005MNO02', 'a0E5f000004JKL02', 48,  'Cooking', '0055f000001USR02', '0055f000001USR02', '2026-02-01T17:00:00', '0055f000001USR02', '2026-02-01T17:00:00', '2026-02-01T17:00:00'),
('a0K5f00000ABCD03', 'CR-00003', 'a0F5f000005MNO03', 'a0E5f000004JKL01', 120, 'Baking',  '0055f000001USR01', '0055f000001USR01', '2026-02-02T08:00:00', '0055f000001USR01', '2026-02-02T08:00:00', '2026-02-02T08:00:00');

-- ============================================================
-- PUBLICATION FOR DEBEZIUM CDC
-- ============================================================
CREATE PUBLICATION salesforce_pub FOR ALL TABLES;
