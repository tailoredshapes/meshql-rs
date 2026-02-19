-- Egg Economy SAP Legacy Database
-- Models a realistic Z-table landscape replicated from an SAP ECC system to PostgreSQL via SLT.
--
-- SAP conventions modeled here:
--   - Z-prefix custom tables (customer namespace)
--   - MANDT (client) on every table, part of composite primary key
--   - Abbreviated German column names (WERKS, STALL_NR, EQUNR, BEHAELT_NR, KUNNR)
--   - Single-character code maps (M/L/H, A/R/D, F/C/B, etc.)
--   - YYYYMMDD dates and HHMMSS times as VARCHAR (not DATE/TIMESTAMP)
--   - Admin fields: ERNAM/ERDAT/ERZET (created), AENAM/AEDAT/AEZET (last changed)
--   - No database-level foreign key constraints (SAP handles RI in ABAP layer)
--   - SAP user names: max 12 chars, uppercase

-- ============================================================
-- FARM MASTER (ZFARM_MSTR)
-- ============================================================
CREATE TABLE ZFARM_MSTR (
    MANDT           VARCHAR(3) NOT NULL DEFAULT '100',  -- SAP client
    WERKS           VARCHAR(4) NOT NULL,                -- Plant code (farm ID)
    BUKRS           VARCHAR(4) NOT NULL DEFAULT '1000', -- Company code
    FARM_NM         VARCHAR(60) NOT NULL,
    FARM_TYP_CD     VARCHAR(1) NOT NULL,                -- M=Megafarm, L=Local, H=Homestead
    ZONE_CD         VARCHAR(30) NOT NULL,
    EIGR            VARCHAR(60),                        -- Owner name (Eigentuemer)
    ERNAM           VARCHAR(12) NOT NULL,               -- Created by (user name)
    ERDAT           VARCHAR(8) NOT NULL,                -- Created date YYYYMMDD
    ERZET           VARCHAR(6) NOT NULL DEFAULT '000000', -- Created time HHMMSS
    AENAM           VARCHAR(12) NOT NULL,               -- Last changed by
    AEDAT           VARCHAR(8) NOT NULL,                -- Last changed date YYYYMMDD
    AEZET           VARCHAR(6) NOT NULL DEFAULT '000000', -- Last changed time HHMMSS
    PRIMARY KEY (MANDT, WERKS)
);

ALTER TABLE ZFARM_MSTR REPLICA IDENTITY FULL;

-- ============================================================
-- STALL MASTER (ZSTALL_MSTR) - Coops
-- ============================================================
CREATE TABLE ZSTALL_MSTR (
    MANDT           VARCHAR(3) NOT NULL DEFAULT '100',
    STALL_NR        VARCHAR(10) NOT NULL,               -- Stall number (coop ID)
    WERKS           VARCHAR(4) NOT NULL,                -- Plant code (FK to farm, no DB constraint)
    STALL_NM        VARCHAR(60) NOT NULL,
    KAPZT           INTEGER NOT NULL,                   -- Kapazitaet (capacity)
    STALL_TYP_CD    VARCHAR(1) NOT NULL,                -- F=Freiland, C=Cage, B=Barn
    ERNAM           VARCHAR(12) NOT NULL,
    ERDAT           VARCHAR(8) NOT NULL,
    ERZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    AENAM           VARCHAR(12) NOT NULL,
    AEDAT           VARCHAR(8) NOT NULL,
    AEZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    PRIMARY KEY (MANDT, STALL_NR)
);

ALTER TABLE ZSTALL_MSTR REPLICA IDENTITY FULL;

-- ============================================================
-- EQUIPMENT HEN (ZEQUI_HEN)
-- ============================================================
CREATE TABLE ZEQUI_HEN (
    MANDT           VARCHAR(3) NOT NULL DEFAULT '100',
    EQUNR           VARCHAR(18) NOT NULL,               -- Equipment number (18-char, padded)
    STALL_NR        VARCHAR(10) NOT NULL,               -- FK to stall (no DB constraint)
    HENNE_NM        VARCHAR(60) NOT NULL,               -- Hen name
    RASSE_CD        VARCHAR(3) NOT NULL,                -- RIR, LEG, PLY, SUS, ORG
    GEB_DT          VARCHAR(8),                         -- Geburtsdatum (birth date) YYYYMMDD
    STAT_CD         VARCHAR(1) DEFAULT 'A',             -- A=Aktiv, R=Ruhestand, D=Dezediert
    ERNAM           VARCHAR(12) NOT NULL,
    ERDAT           VARCHAR(8) NOT NULL,
    ERZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    AENAM           VARCHAR(12) NOT NULL,
    AEDAT           VARCHAR(8) NOT NULL,
    AEZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    PRIMARY KEY (MANDT, EQUNR)
);

ALTER TABLE ZEQUI_HEN REPLICA IDENTITY FULL;

-- ============================================================
-- CONTAINER MASTER (ZBEHAELT_MSTR)
-- ============================================================
CREATE TABLE ZBEHAELT_MSTR (
    MANDT           VARCHAR(3) NOT NULL DEFAULT '100',
    BEHAELT_NR      VARCHAR(10) NOT NULL,               -- Behaelter number (container ID)
    BEHAELT_NM      VARCHAR(60) NOT NULL,
    BEHAELT_TYP_CD  VARCHAR(1) NOT NULL,                -- W=Warehouse, S=Shelf, F=Fridge
    KAPZT           INTEGER NOT NULL,                   -- Kapazitaet
    ZONE_CD         VARCHAR(30) NOT NULL,
    LGORT           VARCHAR(4) NOT NULL DEFAULT '0001', -- Lagerort (storage location)
    ERNAM           VARCHAR(12) NOT NULL,
    ERDAT           VARCHAR(8) NOT NULL,
    ERZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    AENAM           VARCHAR(12) NOT NULL,
    AEDAT           VARCHAR(8) NOT NULL,
    AEZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    PRIMARY KEY (MANDT, BEHAELT_NR)
);

ALTER TABLE ZBEHAELT_MSTR REPLICA IDENTITY FULL;

-- ============================================================
-- CUSTOMER CONSUMER (ZKUNDE_VBR)
-- ============================================================
CREATE TABLE ZKUNDE_VBR (
    MANDT           VARCHAR(3) NOT NULL DEFAULT '100',
    KUNNR           VARCHAR(10) NOT NULL,               -- Kundennummer (10-digit, leading zeros)
    KUND_NM         VARCHAR(60) NOT NULL,
    VBR_TYP_CD      VARCHAR(1) NOT NULL,                -- H=Haushalt, R=Restaurant, B=Baeckerei
    ZONE_CD         VARCHAR(30) NOT NULL,
    WOCH_BEDARF     INTEGER DEFAULT 0,                  -- Wochenbedarf (weekly demand)
    ERNAM           VARCHAR(12) NOT NULL,
    ERDAT           VARCHAR(8) NOT NULL,
    ERZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    AENAM           VARCHAR(12) NOT NULL,
    AEDAT           VARCHAR(8) NOT NULL,
    AEZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    PRIMARY KEY (MANDT, KUNNR)
);

ALTER TABLE ZKUNDE_VBR REPLICA IDENTITY FULL;

-- ============================================================
-- LAY REPORT (ZAFRU_LEGE) — Auftragsrueckmeldung Legevorgang
-- ============================================================
CREATE TABLE ZAFRU_LEGE (
    MANDT           VARCHAR(3) NOT NULL DEFAULT '100',
    AUFNR           VARCHAR(12) NOT NULL,               -- Auftragsnummer (order number)
    EQUNR           VARCHAR(18) NOT NULL,               -- FK to hen equipment
    WERKS           VARCHAR(4) NOT NULL,                -- FK to farm plant
    BUKRS           VARCHAR(4) NOT NULL DEFAULT '1000', -- Company code
    EI_MENGE        INTEGER NOT NULL,                   -- Eier-Menge (egg quantity)
    ERFAS_DT        VARCHAR(8) NOT NULL,                -- Erfassungsdatum (entry date) YYYYMMDD
    ERFAS_ZT        VARCHAR(6),                         -- Erfassungszeit (entry time) HHMMSS
    QUAL_CD         VARCHAR(1) DEFAULT 'A',             -- A=Grade A, B=Grade B, C=Cracked, D=Double yolk
    ERNAM           VARCHAR(12) NOT NULL,
    ERDAT           VARCHAR(8) NOT NULL,
    ERZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    AENAM           VARCHAR(12) NOT NULL,
    AEDAT           VARCHAR(8) NOT NULL,
    AEZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    PRIMARY KEY (MANDT, AUFNR)
);

ALTER TABLE ZAFRU_LEGE REPLICA IDENTITY FULL;

-- ============================================================
-- GOODS RECEIPT - STORAGE DEPOSIT (ZMSEG_101) — Warenbewegung 101
-- ============================================================
CREATE TABLE ZMSEG_101 (
    MANDT           VARCHAR(3) NOT NULL DEFAULT '100',
    MBLNR           VARCHAR(10) NOT NULL,               -- Materialbelegnummer (material doc number)
    MJAHR           VARCHAR(4) NOT NULL DEFAULT '2026', -- Materialbelegjahr (material doc year)
    BEHAELT_NR      VARCHAR(10) NOT NULL,               -- FK to container
    BUKRS           VARCHAR(4) NOT NULL DEFAULT '1000',
    QUELL_TYP_CD    VARCHAR(1) NOT NULL,                -- F=Farm, T=Transfer
    EI_MENGE        INTEGER NOT NULL,
    ERFAS_DT        VARCHAR(8) NOT NULL,
    ERFAS_ZT        VARCHAR(6),
    ERNAM           VARCHAR(12) NOT NULL,
    ERDAT           VARCHAR(8) NOT NULL,
    ERZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    AENAM           VARCHAR(12) NOT NULL,
    AEDAT           VARCHAR(8) NOT NULL,
    AEZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    PRIMARY KEY (MANDT, MBLNR, MJAHR)
);

ALTER TABLE ZMSEG_101 REPLICA IDENTITY FULL;

-- ============================================================
-- GOODS ISSUE - STORAGE WITHDRAWAL (ZMSEG_201) — Warenbewegung 201
-- ============================================================
CREATE TABLE ZMSEG_201 (
    MANDT           VARCHAR(3) NOT NULL DEFAULT '100',
    MBLNR           VARCHAR(10) NOT NULL,
    MJAHR           VARCHAR(4) NOT NULL DEFAULT '2026',
    BEHAELT_NR      VARCHAR(10) NOT NULL,               -- FK to container
    BUKRS           VARCHAR(4) NOT NULL DEFAULT '1000',
    EI_MENGE        INTEGER NOT NULL,
    ERFAS_DT        VARCHAR(8) NOT NULL,
    ERFAS_ZT        VARCHAR(6),
    GRUND_CD        VARCHAR(1) DEFAULT 'S',             -- S=Schwund, B=Bruch, Q=Qualitaetspruefung
    ERNAM           VARCHAR(12) NOT NULL,
    ERDAT           VARCHAR(8) NOT NULL,
    ERZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    AENAM           VARCHAR(12) NOT NULL,
    AEDAT           VARCHAR(8) NOT NULL,
    AEZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    PRIMARY KEY (MANDT, MBLNR, MJAHR)
);

ALTER TABLE ZMSEG_201 REPLICA IDENTITY FULL;

-- ============================================================
-- TRANSFER POSTING (ZMSEG_301) — Warenbewegung 301
-- ============================================================
CREATE TABLE ZMSEG_301 (
    MANDT           VARCHAR(3) NOT NULL DEFAULT '100',
    MBLNR           VARCHAR(10) NOT NULL,
    MJAHR           VARCHAR(4) NOT NULL DEFAULT '2026',
    QUELL_BEH_NR    VARCHAR(10) NOT NULL,               -- Source container
    ZIEL_BEH_NR     VARCHAR(10) NOT NULL,               -- Destination container
    BUKRS           VARCHAR(4) NOT NULL DEFAULT '1000',
    EI_MENGE        INTEGER NOT NULL,
    ERFAS_DT        VARCHAR(8) NOT NULL,
    ERFAS_ZT        VARCHAR(6),
    TRANS_CD        VARCHAR(1) DEFAULT 'T',             -- T=Truck, V=Van, K=Karren
    ERNAM           VARCHAR(12) NOT NULL,
    ERDAT           VARCHAR(8) NOT NULL,
    ERZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    AENAM           VARCHAR(12) NOT NULL,
    AEDAT           VARCHAR(8) NOT NULL,
    AEZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    PRIMARY KEY (MANDT, MBLNR, MJAHR)
);

ALTER TABLE ZMSEG_301 REPLICA IDENTITY FULL;

-- ============================================================
-- CONSUMPTION POSTING (ZMSEG_261) — Warenbewegung 261
-- ============================================================
CREATE TABLE ZMSEG_261 (
    MANDT           VARCHAR(3) NOT NULL DEFAULT '100',
    MBLNR           VARCHAR(10) NOT NULL,
    MJAHR           VARCHAR(4) NOT NULL DEFAULT '2026',
    KUNNR           VARCHAR(10) NOT NULL,               -- FK to customer
    BEHAELT_NR      VARCHAR(10) NOT NULL,               -- FK to container
    BUKRS           VARCHAR(4) NOT NULL DEFAULT '1000',
    EI_MENGE        INTEGER NOT NULL,
    ERFAS_DT        VARCHAR(8) NOT NULL,
    ERFAS_ZT        VARCHAR(6),
    ZWECK_CD        VARCHAR(1) DEFAULT 'K',             -- K=Kochen, B=Backen, W=Weiterverkauf, R=Roh
    ERNAM           VARCHAR(12) NOT NULL,
    ERDAT           VARCHAR(8) NOT NULL,
    ERZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    AENAM           VARCHAR(12) NOT NULL,
    AEDAT           VARCHAR(8) NOT NULL,
    AEZET           VARCHAR(6) NOT NULL DEFAULT '000000',
    PRIMARY KEY (MANDT, MBLNR, MJAHR)
);

ALTER TABLE ZMSEG_261 REPLICA IDENTITY FULL;

-- ============================================================
-- SEED DATA
-- ============================================================

-- SAP users (SY-UNAME format: max 12 chars, uppercase)
-- HMUELLE  = Heinrich Mueller (Basis Admin)
-- ASCHMIDT = Anna Schmidt (Key User)
-- KWEBER   = Klaus Weber (Key User)

-- Farms (WERKS: 4-char plant code, standard SAP format)
INSERT INTO ZFARM_MSTR (MANDT, WERKS, BUKRS, FARM_NM, FARM_TYP_CD, ZONE_CD, EIGR, ERNAM, ERDAT, ERZET, AENAM, AEDAT, AEZET) VALUES
('100', '1000', '1000', 'Sonnenhof Megafarm',   'M', 'north', 'Heinrich Mueller', 'HMUELLE',  '20150301', '080000', 'HMUELLE',  '20241115', '142200'),
('100', '2000', '1000', 'Gruener Hof',           'L', 'south', 'Anna Schmidt',     'ASCHMIDT', '20180615', '093000', 'ASCHMIDT', '20250820', '110500'),
('100', '3000', '1000', 'Berghof Homestead',     'H', 'east',  'Klaus Weber',      'HMUELLE',  '20200101', '070000', 'KWEBER',   '20200101', '070000');

-- Coops (STALL_NR: custom number range)
INSERT INTO ZSTALL_MSTR (MANDT, STALL_NR, WERKS, STALL_NM, KAPZT, STALL_TYP_CD, ERNAM, ERDAT, ERZET, AENAM, AEDAT, AEZET) VALUES
('100', '0000000001', '1000', 'Mega Stall Alpha', 500, 'C', 'HMUELLE',  '20150401', '100000', 'HMUELLE',  '20230310', '091500'),
('100', '0000000002', '1000', 'Mega Stall Beta',  500, 'C', 'HMUELLE',  '20150415', '100000', 'HMUELLE',  '20150415', '100000'),
('100', '0000000003', '2000', 'Freiland Stall',   100, 'F', 'ASCHMIDT', '20180701', '110000', 'ASCHMIDT', '20180701', '110000'),
('100', '0000000004', '3000', 'Berg Stall',        25, 'B', 'KWEBER',   '20200201', '080000', 'KWEBER',   '20200201', '080000');

-- Hens (EQUNR: 18-char equipment number, leading zeros — standard SAP format)
INSERT INTO ZEQUI_HEN (MANDT, EQUNR, STALL_NR, HENNE_NM, RASSE_CD, GEB_DT, STAT_CD, ERNAM, ERDAT, ERZET, AENAM, AEDAT, AEZET) VALUES
('100', '000000000000000001', '0000000001', 'Henne Greta',  'RIR', '20230115', 'A', 'HMUELLE',  '20230115', '060000', 'HMUELLE',  '20230115', '060000'),
('100', '000000000000000002', '0000000001', 'Henne Liesel', 'LEG', '20230220', 'A', 'HMUELLE',  '20230220', '060000', 'HMUELLE',  '20230220', '060000'),
('100', '000000000000000003', '0000000002', 'Henne Berta',  'PLY', '20220801', 'A', 'HMUELLE',  '20220801', '060000', 'HMUELLE',  '20220801', '060000'),
('100', '000000000000000004', '0000000003', 'Henne Frieda', 'SUS', '20230601', 'A', 'ASCHMIDT', '20230601', '060000', 'ASCHMIDT', '20230601', '060000'),
('100', '000000000000000005', '0000000003', 'Henne Klara',  'RIR', '20230915', 'A', 'ASCHMIDT', '20230915', '060000', 'ASCHMIDT', '20230915', '060000'),
('100', '000000000000000006', '0000000004', 'Henne Rosa',   'ORG', '20240101', 'A', 'KWEBER',   '20240101', '060000', 'KWEBER',   '20240101', '060000'),
('100', '000000000000000007', '0000000001', 'Henne Elsa',   'LEG', '20220315', 'R', 'HMUELLE',  '20220315', '060000', 'HMUELLE',  '20251201', '103000');

-- Containers (BEHAELT_NR: custom number range)
INSERT INTO ZBEHAELT_MSTR (MANDT, BEHAELT_NR, BEHAELT_NM, BEHAELT_TYP_CD, KAPZT, ZONE_CD, LGORT, ERNAM, ERDAT, ERZET, AENAM, AEDAT, AEZET) VALUES
('100', '0000000001', 'Nordlager',         'W', 10000, 'north', '0001', 'HMUELLE',  '20150101', '080000', 'HMUELLE',  '20150101', '080000'),
('100', '0000000002', 'Suedlager',         'W', 5000,  'south', '0002', 'ASCHMIDT', '20180101', '080000', 'ASCHMIDT', '20180101', '080000'),
('100', '0000000003', 'Markt Regal Nord',  'S', 200,   'north', '0001', 'HMUELLE',  '20190601', '080000', 'HMUELLE',  '20190601', '080000'),
('100', '0000000004', 'Kuehlschrank Ost',  'F', 50,    'east',  '0003', 'KWEBER',   '20200301', '080000', 'KWEBER',   '20200301', '080000');

-- Consumers (KUNNR: 10-digit customer number, leading zeros — standard SAP format)
INSERT INTO ZKUNDE_VBR (MANDT, KUNNR, KUND_NM, VBR_TYP_CD, ZONE_CD, WOCH_BEDARF, ERNAM, ERDAT, ERZET, AENAM, AEDAT, AEZET) VALUES
('100', '0000100001', 'Familie Braun',       'H', 'north', 12,  'HMUELLE',  '20200101', '080000', 'HMUELLE',  '20200101', '080000'),
('100', '0000100002', 'Gasthaus zum Adler',  'R', 'south', 200, 'ASCHMIDT', '20190601', '080000', 'ASCHMIDT', '20250110', '164500'),
('100', '0000100003', 'Baeckerei Meier',     'B', 'north', 500, 'HMUELLE',  '20180301', '080000', 'ASCHMIDT', '20240601', '090000');

-- Lay Reports (AUFNR: 12-digit order number)
INSERT INTO ZAFRU_LEGE (MANDT, AUFNR, EQUNR, WERKS, BUKRS, EI_MENGE, ERFAS_DT, ERFAS_ZT, QUAL_CD, ERNAM, ERDAT, ERZET, AENAM, AEDAT, AEZET) VALUES
('100', '000001000001', '000000000000000001', '1000', '1000', 3, '20260201', '063000', 'A', 'HMUELLE',  '20260201', '063000', 'HMUELLE',  '20260201', '063000'),
('100', '000001000002', '000000000000000002', '1000', '1000', 2, '20260201', '064500', 'A', 'HMUELLE',  '20260201', '064500', 'HMUELLE',  '20260201', '064500'),
('100', '000001000003', '000000000000000003', '1000', '1000', 4, '20260201', '070000', 'D', 'HMUELLE',  '20260201', '070000', 'HMUELLE',  '20260201', '070000'),
('100', '000001000004', '000000000000000004', '2000', '1000', 2, '20260201', '071500', 'A', 'ASCHMIDT', '20260201', '071500', 'ASCHMIDT', '20260201', '071500'),
('100', '000001000005', '000000000000000005', '2000', '1000', 1, '20260201', '073000', 'B', 'ASCHMIDT', '20260201', '073000', 'ASCHMIDT', '20260201', '073000'),
('100', '000001000006', '000000000000000006', '3000', '1000', 1, '20260201', '080000', 'A', 'KWEBER',   '20260201', '080000', 'KWEBER',   '20260201', '080000'),
('100', '000001000007', '000000000000000001', '1000', '1000', 4, '20260202', '063000', 'A', 'HMUELLE',  '20260202', '063000', 'HMUELLE',  '20260202', '063000'),
('100', '000001000008', '000000000000000004', '2000', '1000', 3, '20260202', '071500', 'A', 'ASCHMIDT', '20260202', '071500', 'ASCHMIDT', '20260202', '071500');

-- Storage Deposits (MBLNR: 10-digit material doc, MJAHR: document year)
INSERT INTO ZMSEG_101 (MANDT, MBLNR, MJAHR, BEHAELT_NR, BUKRS, QUELL_TYP_CD, EI_MENGE, ERFAS_DT, ERFAS_ZT, ERNAM, ERDAT, ERZET, AENAM, AEDAT, AEZET) VALUES
('100', '4900000001', '2026', '0000000001', '1000', 'F', 9, '20260201', '100000', 'HMUELLE',  '20260201', '100000', 'HMUELLE',  '20260201', '100000'),
('100', '4900000002', '2026', '0000000002', '1000', 'F', 3, '20260201', '110000', 'ASCHMIDT', '20260201', '110000', 'ASCHMIDT', '20260201', '110000'),
('100', '4900000003', '2026', '0000000004', '1000', 'F', 1, '20260201', '120000', 'KWEBER',   '20260201', '120000', 'KWEBER',   '20260201', '120000'),
('100', '4900000004', '2026', '0000000001', '1000', 'F', 7, '20260202', '100000', 'HMUELLE',  '20260202', '100000', 'HMUELLE',  '20260202', '100000');

-- Storage Withdrawals
INSERT INTO ZMSEG_201 (MANDT, MBLNR, MJAHR, BEHAELT_NR, BUKRS, EI_MENGE, ERFAS_DT, ERFAS_ZT, GRUND_CD, ERNAM, ERDAT, ERZET, AENAM, AEDAT, AEZET) VALUES
('100', '4900000005', '2026', '0000000001', '1000', 2, '20260201', '140000', 'S', 'HMUELLE',  '20260201', '140000', 'HMUELLE',  '20260201', '140000'),
('100', '4900000006', '2026', '0000000002', '1000', 1, '20260201', '150000', 'B', 'ASCHMIDT', '20260201', '150000', 'ASCHMIDT', '20260201', '150000');

-- Container Transfers
INSERT INTO ZMSEG_301 (MANDT, MBLNR, MJAHR, QUELL_BEH_NR, ZIEL_BEH_NR, BUKRS, EI_MENGE, ERFAS_DT, ERFAS_ZT, TRANS_CD, ERNAM, ERDAT, ERZET, AENAM, AEDAT, AEZET) VALUES
('100', '4900000007', '2026', '0000000001', '0000000003', '1000', 24, '20260201', '160000', 'V', 'HMUELLE',  '20260201', '160000', 'HMUELLE',  '20260201', '160000'),
('100', '4900000008', '2026', '0000000002', '0000000004', '1000', 6,  '20260202', '090000', 'K', 'ASCHMIDT', '20260202', '090000', 'ASCHMIDT', '20260202', '090000');

-- Consumption Reports
INSERT INTO ZMSEG_261 (MANDT, MBLNR, MJAHR, KUNNR, BEHAELT_NR, BUKRS, EI_MENGE, ERFAS_DT, ERFAS_ZT, ZWECK_CD, ERNAM, ERDAT, ERZET, AENAM, AEDAT, AEZET) VALUES
('100', '4900000009', '2026', '0000100001', '0000000003', '1000', 12,  '20260201', '180000', 'K', 'HMUELLE',  '20260201', '180000', 'HMUELLE',  '20260201', '180000'),
('100', '4900000010', '2026', '0000100002', '0000000002', '1000', 48,  '20260201', '170000', 'K', 'ASCHMIDT', '20260201', '170000', 'ASCHMIDT', '20260201', '170000'),
('100', '4900000011', '2026', '0000100003', '0000000001', '1000', 120, '20260202', '080000', 'B', 'HMUELLE',  '20260202', '080000', 'HMUELLE',  '20260202', '080000');

-- ============================================================
-- PUBLICATION FOR DEBEZIUM CDC
-- ============================================================
CREATE PUBLICATION sap_pub FOR ALL TABLES;
