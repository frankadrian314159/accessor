//! TPC-DS queries, with the substitution parameters of the specification's qualification run where the generated data has them.
//! These are a selection of the 99 that cover the features the benchmark is built to exercise: common table expressions used
//! more than once, `ROLLUP` and `GROUPING`, window functions over aggregates, `INTERSECT` / `EXCEPT`, correlated and
//! uncorrelated subqueries, `UNION ALL` of the three sales channels, and many-way star joins.

pub fn queries() -> Vec<(u32, &'static str)> {
    vec![
        (1, "WITH customer_total_return AS (SELECT sr_customer_sk AS ctr_customer_sk, sr_store_sk AS ctr_store_sk, SUM(sr_return_amt) AS ctr_total_return
  FROM store_returns, date_dim WHERE sr_returned_date_sk = d_date_sk AND d_year = 2000 GROUP BY sr_customer_sk, sr_store_sk)
SELECT c_customer_id FROM customer_total_return ctr1, store, customer
WHERE ctr1.ctr_total_return > (SELECT AVG(ctr_total_return) * 1.2 FROM customer_total_return ctr2 WHERE ctr1.ctr_store_sk = ctr2.ctr_store_sk)
  AND s_store_sk = ctr1.ctr_store_sk AND s_state = 'TN' AND ctr1.ctr_customer_sk = c_customer_sk ORDER BY c_customer_id LIMIT 100"),
        (3, "SELECT dt.d_year, item.i_brand_id AS brand_id, item.i_brand AS brand, SUM(ss_ext_sales_price) AS sum_agg
FROM date_dim dt, store_sales, item WHERE dt.d_date_sk = store_sales.ss_sold_date_sk AND store_sales.ss_item_sk = item.i_item_sk
  AND item.i_manufact_id = 128 AND dt.d_moy = 11 GROUP BY dt.d_year, item.i_brand, item.i_brand_id
ORDER BY dt.d_year, sum_agg DESC, brand_id LIMIT 100"),
        (6, "SELECT a.ca_state AS state, COUNT(*) AS cnt FROM customer_address a, customer c, store_sales s, date_dim d, item i
WHERE a.ca_address_sk = c.c_current_addr_sk AND c.c_customer_sk = s.ss_customer_sk AND s.ss_sold_date_sk = d.d_date_sk AND s.ss_item_sk = i.i_item_sk
  AND d.d_month_seq = (SELECT DISTINCT d_month_seq FROM date_dim WHERE d_year = 2001 AND d_moy = 1)
  AND i.i_current_price > 1.2 * (SELECT AVG(j.i_current_price) FROM item j WHERE j.i_category = i.i_category)
GROUP BY a.ca_state HAVING COUNT(*) >= 10 ORDER BY cnt, a.ca_state LIMIT 100"),
        (7, "SELECT i_item_id, AVG(ss_quantity) agg1, AVG(ss_list_price) agg2, AVG(ss_coupon_amt) agg3, AVG(ss_sales_price) agg4
FROM store_sales, customer_demographics, date_dim, item, promotion
WHERE ss_sold_date_sk = d_date_sk AND ss_item_sk = i_item_sk AND ss_cdemo_sk = cd_demo_sk AND ss_promo_sk = p_promo_sk
  AND cd_gender = 'M' AND cd_marital_status = 'S' AND cd_education_status = 'College' AND (p_channel_email = 'N' OR p_channel_event = 'N') AND d_year = 2000
GROUP BY i_item_id ORDER BY i_item_id LIMIT 100"),
        (11, "WITH year_total AS (
  SELECT c_customer_id customer_id, c_first_name customer_first_name, c_last_name customer_last_name, d_year dyear,
    SUM(ss_ext_list_price - ss_ext_discount_amt) year_total, 's' sale_type
  FROM customer, store_sales, date_dim WHERE c_customer_sk = ss_customer_sk AND ss_sold_date_sk = d_date_sk GROUP BY c_customer_id, c_first_name, c_last_name, d_year
  UNION ALL
  SELECT c_customer_id, c_first_name, c_last_name, d_year, SUM(ws_ext_list_price - ws_ext_discount_amt), 'w'
  FROM customer, web_sales, date_dim WHERE c_customer_sk = ws_bill_customer_sk AND ws_sold_date_sk = d_date_sk GROUP BY c_customer_id, c_first_name, c_last_name, d_year)
SELECT t_s_secyear.customer_id, t_s_secyear.customer_first_name, t_s_secyear.customer_last_name
FROM year_total t_s_firstyear, year_total t_s_secyear, year_total t_w_firstyear, year_total t_w_secyear
WHERE t_s_secyear.customer_id = t_s_firstyear.customer_id AND t_s_firstyear.customer_id = t_w_secyear.customer_id AND t_s_firstyear.customer_id = t_w_firstyear.customer_id
  AND t_s_firstyear.sale_type = 's' AND t_w_firstyear.sale_type = 'w' AND t_s_secyear.sale_type = 's' AND t_w_secyear.sale_type = 'w'
  AND t_s_firstyear.dyear = 2001 AND t_s_secyear.dyear = 2002 AND t_w_firstyear.dyear = 2001 AND t_w_secyear.dyear = 2002
  AND t_s_firstyear.year_total > 0 AND t_w_firstyear.year_total > 0
  AND CASE WHEN t_w_firstyear.year_total > 0 THEN t_w_secyear.year_total / t_w_firstyear.year_total ELSE 0.0 END
    > CASE WHEN t_s_firstyear.year_total > 0 THEN t_s_secyear.year_total / t_s_firstyear.year_total ELSE 0.0 END
ORDER BY t_s_secyear.customer_id, t_s_secyear.customer_first_name, t_s_secyear.customer_last_name LIMIT 100"),
        (12, "SELECT i_item_id, i_item_desc, i_category, i_class, i_current_price, SUM(ws_ext_sales_price) AS itemrevenue,
  SUM(ws_ext_sales_price) * 100 / SUM(SUM(ws_ext_sales_price)) OVER (PARTITION BY i_class) AS revenueratio
FROM web_sales, item, date_dim WHERE ws_item_sk = i_item_sk AND i_category IN ('Sports', 'Books', 'Home') AND ws_sold_date_sk = d_date_sk
  AND d_date BETWEEN DATE '1999-02-22' AND DATE '1999-03-24'
GROUP BY i_item_id, i_item_desc, i_category, i_class, i_current_price ORDER BY i_category, i_class, i_item_id, i_item_desc, revenueratio LIMIT 100"),
        (15, "SELECT ca_zip, SUM(cs_sales_price) FROM catalog_sales, customer, customer_address, date_dim
WHERE cs_bill_customer_sk = c_customer_sk AND c_current_addr_sk = ca_address_sk
  AND (SUBSTR(ca_zip, 1, 5) IN ('85669', '86197', '88274', '83405', '86475', '85392', '85460', '80348', '81792') OR ca_state IN ('CA', 'WA', 'GA') OR cs_sales_price > 500)
  AND cs_sold_date_sk = d_date_sk AND d_qoy = 2 AND d_year = 2001 GROUP BY ca_zip ORDER BY ca_zip LIMIT 100"),
        (19, "SELECT i_brand_id brand_id, i_brand brand, i_manufact_id, i_manufact, SUM(ss_ext_sales_price) ext_price
FROM date_dim, store_sales, item, customer, customer_address, store
WHERE d_date_sk = ss_sold_date_sk AND ss_item_sk = i_item_sk AND i_manager_id = 8 AND d_moy = 11 AND d_year = 1998 AND ss_customer_sk = c_customer_sk
  AND c_current_addr_sk = ca_address_sk AND SUBSTR(ca_zip, 1, 5) <> SUBSTR(s_zip, 1, 5) AND ss_store_sk = s_store_sk
GROUP BY i_brand, i_brand_id, i_manufact_id, i_manufact ORDER BY ext_price DESC, i_brand, i_brand_id, i_manufact_id, i_manufact LIMIT 100"),
        (20, "SELECT i_item_id, i_item_desc, i_category, i_class, i_current_price, SUM(cs_ext_sales_price) AS itemrevenue,
  SUM(cs_ext_sales_price) * 100 / SUM(SUM(cs_ext_sales_price)) OVER (PARTITION BY i_class) AS revenueratio
FROM catalog_sales, item, date_dim WHERE cs_item_sk = i_item_sk AND i_category IN ('Sports', 'Books', 'Home') AND cs_sold_date_sk = d_date_sk
  AND d_date BETWEEN DATE '1999-02-22' AND DATE '1999-03-24'
GROUP BY i_item_id, i_item_desc, i_category, i_class, i_current_price ORDER BY i_category, i_class, i_item_id, i_item_desc, revenueratio LIMIT 100"),
        (22, "SELECT i_product_name, i_brand, i_class, i_category, AVG(inv_quantity_on_hand) qoh FROM inventory, date_dim, item
WHERE inv_date_sk = d_date_sk AND inv_item_sk = i_item_sk AND d_month_seq BETWEEN 1200 AND 1200 + 11
GROUP BY ROLLUP (i_product_name, i_brand, i_class, i_category) ORDER BY qoh, i_product_name, i_brand, i_class, i_category LIMIT 100"),
        (25, "SELECT i_item_id, i_item_desc, s_store_id, s_store_name, MAX(ss_net_profit) AS store_sales_profit, MAX(sr_net_loss) AS store_returns_loss, MAX(cs_net_profit) AS catalog_sales_profit
FROM store_sales, store_returns, catalog_sales, date_dim d1, date_dim d2, date_dim d3, store, item
WHERE d1.d_moy = 4 AND d1.d_year = 2001 AND d1.d_date_sk = ss_sold_date_sk AND i_item_sk = ss_item_sk AND s_store_sk = ss_store_sk
  AND ss_customer_sk = sr_customer_sk AND ss_item_sk = sr_item_sk AND ss_ticket_number = sr_ticket_number AND sr_returned_date_sk = d2.d_date_sk
  AND d2.d_moy BETWEEN 4 AND 10 AND d2.d_year = 2001 AND sr_customer_sk = cs_bill_customer_sk AND sr_item_sk = cs_item_sk AND cs_sold_date_sk = d3.d_date_sk
  AND d3.d_moy BETWEEN 4 AND 10 AND d3.d_year = 2001
GROUP BY i_item_id, i_item_desc, s_store_id, s_store_name ORDER BY i_item_id, i_item_desc, s_store_id, s_store_name LIMIT 100"),
        (26, "SELECT i_item_id, AVG(cs_quantity) agg1, AVG(cs_list_price) agg2, AVG(cs_coupon_amt) agg3, AVG(cs_sales_price) agg4
FROM catalog_sales, customer_demographics, date_dim, item, promotion
WHERE cs_sold_date_sk = d_date_sk AND cs_item_sk = i_item_sk AND cs_bill_cdemo_sk = cd_demo_sk AND cs_promo_sk = p_promo_sk
  AND cd_gender = 'M' AND cd_marital_status = 'S' AND cd_education_status = 'College' AND (p_channel_email = 'N' OR p_channel_event = 'N') AND d_year = 2000
GROUP BY i_item_id ORDER BY i_item_id LIMIT 100"),
        (27, "SELECT i_item_id, s_state, GROUPING(s_state) g_state, AVG(ss_quantity) agg1, AVG(ss_list_price) agg2, AVG(ss_coupon_amt) agg3, AVG(ss_sales_price) agg4
FROM store_sales, customer_demographics, date_dim, store, item
WHERE ss_sold_date_sk = d_date_sk AND ss_item_sk = i_item_sk AND ss_store_sk = s_store_sk AND ss_cdemo_sk = cd_demo_sk
  AND cd_gender = 'M' AND cd_marital_status = 'S' AND cd_education_status = 'College' AND d_year = 2002 AND s_state IN ('TN', 'SD', 'AL', 'SC', 'OH', 'GA')
GROUP BY ROLLUP (i_item_id, s_state) ORDER BY i_item_id, s_state LIMIT 100"),
        (29, "SELECT i_item_id, i_item_desc, s_store_id, s_store_name, SUM(ss_quantity) AS store_sales_quantity, SUM(sr_return_quantity) AS store_returns_quantity, SUM(cs_quantity) AS catalog_sales_quantity
FROM store_sales, store_returns, catalog_sales, date_dim d1, date_dim d2, date_dim d3, store, item
WHERE d1.d_moy = 9 AND d1.d_year = 1999 AND d1.d_date_sk = ss_sold_date_sk AND i_item_sk = ss_item_sk AND s_store_sk = ss_store_sk
  AND ss_customer_sk = sr_customer_sk AND ss_item_sk = sr_item_sk AND ss_ticket_number = sr_ticket_number AND sr_returned_date_sk = d2.d_date_sk
  AND d2.d_moy BETWEEN 9 AND 12 AND d2.d_year = 1999 AND sr_customer_sk = cs_bill_customer_sk AND sr_item_sk = cs_item_sk AND cs_sold_date_sk = d3.d_date_sk
  AND d3.d_year IN (1999, 2000, 2001)
GROUP BY i_item_id, i_item_desc, s_store_id, s_store_name ORDER BY i_item_id, i_item_desc, s_store_id, s_store_name LIMIT 100"),
        (34, "SELECT c_last_name, c_first_name, c_salutation, c_preferred_cust_flag, ss_ticket_number, cnt FROM (
  SELECT ss_ticket_number, ss_customer_sk, COUNT(*) cnt FROM store_sales, date_dim, store, household_demographics
  WHERE store_sales.ss_sold_date_sk = date_dim.d_date_sk AND store_sales.ss_store_sk = store.s_store_sk AND store_sales.ss_hdemo_sk = household_demographics.hd_demo_sk
    AND (date_dim.d_dom BETWEEN 1 AND 3 OR date_dim.d_dom BETWEEN 25 AND 28) AND (household_demographics.hd_buy_potential = '>10000' OR household_demographics.hd_buy_potential = 'Unknown')
    AND household_demographics.hd_vehicle_count > 0
    AND (CASE WHEN household_demographics.hd_vehicle_count > 0 THEN household_demographics.hd_dep_count / household_demographics.hd_vehicle_count ELSE NULL END) > 1.2
    AND date_dim.d_year IN (1999, 2000, 2001) AND store.s_county IN ('Williamson County', 'Franklin Parish', 'Bronx County', 'Orange County')
  GROUP BY ss_ticket_number, ss_customer_sk) dn, customer
WHERE ss_customer_sk = c_customer_sk AND cnt BETWEEN 15 AND 20 ORDER BY c_last_name, c_first_name, c_salutation, c_preferred_cust_flag DESC, ss_ticket_number"),
        (36, "SELECT SUM(ss_net_profit) / SUM(ss_ext_sales_price) AS gross_margin, i_category, i_class, GROUPING(i_category) + GROUPING(i_class) AS lochierarchy,
  RANK() OVER (PARTITION BY GROUPING(i_category) + GROUPING(i_class), CASE WHEN GROUPING(i_class) = 0 THEN i_category END ORDER BY SUM(ss_net_profit) / SUM(ss_ext_sales_price) ASC) AS rank_within_parent
FROM store_sales, date_dim d1, item, store
WHERE d1.d_year = 2001 AND d1.d_date_sk = ss_sold_date_sk AND i_item_sk = ss_item_sk AND s_store_sk = ss_store_sk AND s_state IN ('TN', 'SD', 'AL', 'SC', 'OH', 'GA', 'MI', 'IL')
GROUP BY ROLLUP (i_category, i_class)
ORDER BY lochierarchy DESC, CASE WHEN lochierarchy = 0 THEN i_category END, rank_within_parent LIMIT 100"),
        (38, "SELECT COUNT(*) FROM (
  SELECT DISTINCT c_last_name, c_first_name, d_date FROM store_sales, date_dim, customer WHERE store_sales.ss_sold_date_sk = date_dim.d_date_sk AND store_sales.ss_customer_sk = customer.c_customer_sk AND d_month_seq BETWEEN 1200 AND 1200 + 11
  INTERSECT
  SELECT DISTINCT c_last_name, c_first_name, d_date FROM catalog_sales, date_dim, customer WHERE catalog_sales.cs_sold_date_sk = date_dim.d_date_sk AND catalog_sales.cs_bill_customer_sk = customer.c_customer_sk AND d_month_seq BETWEEN 1200 AND 1200 + 11
  INTERSECT
  SELECT DISTINCT c_last_name, c_first_name, d_date FROM web_sales, date_dim, customer WHERE web_sales.ws_sold_date_sk = date_dim.d_date_sk AND web_sales.ws_bill_customer_sk = customer.c_customer_sk AND d_month_seq BETWEEN 1200 AND 1200 + 11) hot_cust LIMIT 100"),
        (42, "SELECT dt.d_year, item.i_category_id, item.i_category, SUM(ss_ext_sales_price) FROM date_dim dt, store_sales, item
WHERE dt.d_date_sk = store_sales.ss_sold_date_sk AND store_sales.ss_item_sk = item.i_item_sk AND item.i_manager_id = 1 AND dt.d_moy = 11 AND dt.d_year = 2000
GROUP BY dt.d_year, item.i_category_id, item.i_category ORDER BY SUM(ss_ext_sales_price) DESC, dt.d_year, item.i_category_id, item.i_category LIMIT 100"),
        (43, "SELECT s_store_name, s_store_id,
  SUM(CASE WHEN d_day_name = 'Sunday' THEN ss_sales_price ELSE NULL END) sun_sales, SUM(CASE WHEN d_day_name = 'Monday' THEN ss_sales_price ELSE NULL END) mon_sales,
  SUM(CASE WHEN d_day_name = 'Tuesday' THEN ss_sales_price ELSE NULL END) tue_sales, SUM(CASE WHEN d_day_name = 'Wednesday' THEN ss_sales_price ELSE NULL END) wed_sales,
  SUM(CASE WHEN d_day_name = 'Thursday' THEN ss_sales_price ELSE NULL END) thu_sales, SUM(CASE WHEN d_day_name = 'Friday' THEN ss_sales_price ELSE NULL END) fri_sales,
  SUM(CASE WHEN d_day_name = 'Saturday' THEN ss_sales_price ELSE NULL END) sat_sales
FROM date_dim, store_sales, store WHERE d_date_sk = ss_sold_date_sk AND s_store_sk = ss_store_sk AND s_gmt_offset = -5 AND d_year = 2000
GROUP BY s_store_name, s_store_id ORDER BY s_store_name, s_store_id, sun_sales, mon_sales, tue_sales, wed_sales, thu_sales, fri_sales, sat_sales LIMIT 100"),
        (46, "SELECT c_last_name, c_first_name, ca_city, bought_city, ss_ticket_number, amt, profit FROM (
  SELECT ss_ticket_number, ss_customer_sk, ca_city bought_city, SUM(ss_coupon_amt) amt, SUM(ss_net_profit) profit
  FROM store_sales, date_dim, store, household_demographics, customer_address
  WHERE store_sales.ss_sold_date_sk = date_dim.d_date_sk AND store_sales.ss_store_sk = store.s_store_sk AND store_sales.ss_hdemo_sk = household_demographics.hd_demo_sk
    AND store_sales.ss_addr_sk = customer_address.ca_address_sk AND (household_demographics.hd_dep_count = 4 OR household_demographics.hd_vehicle_count = 3)
    AND date_dim.d_dow IN (6, 0) AND date_dim.d_year IN (1999, 2000, 2001) AND store.s_city IN ('Fairview', 'Midway', 'Fairview', 'Fairview', 'Fairview')
  GROUP BY ss_ticket_number, ss_customer_sk, ss_addr_sk, ca_city) dn, customer, customer_address current_addr
WHERE ss_customer_sk = c_customer_sk AND customer.c_current_addr_sk = current_addr.ca_address_sk AND current_addr.ca_city <> bought_city
ORDER BY c_last_name, c_first_name, ca_city, bought_city, ss_ticket_number LIMIT 100"),
        (48, "SELECT SUM(ss_quantity) FROM store_sales, store, customer_demographics, customer_address, date_dim
WHERE s_store_sk = ss_store_sk AND ss_sold_date_sk = d_date_sk AND d_year = 2000
  AND ((cd_demo_sk = ss_cdemo_sk AND cd_marital_status = 'M' AND cd_education_status = '4 yr Degree' AND ss_sales_price BETWEEN 100.00 AND 150.00)
    OR (cd_demo_sk = ss_cdemo_sk AND cd_marital_status = 'D' AND cd_education_status = '2 yr Degree' AND ss_sales_price BETWEEN 50.00 AND 100.00)
    OR (cd_demo_sk = ss_cdemo_sk AND cd_marital_status = 'S' AND cd_education_status = 'College' AND ss_sales_price BETWEEN 150.00 AND 200.00))
  AND ((ss_addr_sk = ca_address_sk AND ca_country = 'United States' AND ca_state IN ('CO', 'OH', 'TX') AND ss_net_profit BETWEEN 0 AND 2000)
    OR (ss_addr_sk = ca_address_sk AND ca_country = 'United States' AND ca_state IN ('OR', 'MN', 'KY') AND ss_net_profit BETWEEN 150 AND 3000)
    OR (ss_addr_sk = ca_address_sk AND ca_country = 'United States' AND ca_state IN ('VA', 'CA', 'MS') AND ss_net_profit BETWEEN 50 AND 25000))"),
        (52, "SELECT dt.d_year, item.i_brand_id brand_id, item.i_brand brand, SUM(ss_ext_sales_price) ext_price FROM date_dim dt, store_sales, item
WHERE dt.d_date_sk = store_sales.ss_sold_date_sk AND store_sales.ss_item_sk = item.i_item_sk AND item.i_manager_id = 1 AND dt.d_moy = 11 AND dt.d_year = 2000
GROUP BY dt.d_year, item.i_brand, item.i_brand_id ORDER BY dt.d_year, ext_price DESC, brand_id LIMIT 100"),
        (53, "SELECT * FROM (SELECT i_manufact_id, SUM(ss_sales_price) sum_sales, AVG(SUM(ss_sales_price)) OVER (PARTITION BY i_manufact_id) avg_quarterly_sales
  FROM item, store_sales, date_dim, store WHERE ss_item_sk = i_item_sk AND ss_sold_date_sk = d_date_sk AND ss_store_sk = s_store_sk
    AND d_month_seq IN (1200, 1201, 1202, 1203, 1204, 1205, 1206, 1207, 1208, 1209, 1210, 1211)
    AND ((i_category IN ('Books', 'Children', 'Electronics') AND i_class IN ('personal', 'portable', 'reference', 'self-help'))
      OR (i_category IN ('Women', 'Music', 'Men') AND i_class IN ('accessories', 'classical', 'fragrances', 'pants')))
  GROUP BY i_manufact_id, d_qoy) tmp1
WHERE CASE WHEN avg_quarterly_sales > 0 THEN ABS(sum_sales - avg_quarterly_sales) / avg_quarterly_sales ELSE NULL END > 0.1
ORDER BY avg_quarterly_sales, sum_sales, i_manufact_id LIMIT 100"),
        (55, "SELECT i_brand_id brand_id, i_brand brand, SUM(ss_ext_sales_price) ext_price FROM date_dim, store_sales, item
WHERE d_date_sk = ss_sold_date_sk AND ss_item_sk = i_item_sk AND i_manager_id = 28 AND d_moy = 11 AND d_year = 1999
GROUP BY i_brand, i_brand_id ORDER BY ext_price DESC, i_brand_id LIMIT 100"),
        (56, "WITH ss AS (SELECT i_item_id, SUM(ss_ext_sales_price) total_sales FROM store_sales, date_dim, customer_address, item
    WHERE i_item_id IN (SELECT i_item_id FROM item WHERE i_color IN ('slate', 'blanched', 'burnished')) AND ss_item_sk = i_item_sk AND ss_sold_date_sk = d_date_sk
      AND d_year = 2001 AND d_moy = 2 AND ss_addr_sk = ca_address_sk AND ca_gmt_offset = -5 GROUP BY i_item_id),
  cs AS (SELECT i_item_id, SUM(cs_ext_sales_price) total_sales FROM catalog_sales, date_dim, customer_address, item
    WHERE i_item_id IN (SELECT i_item_id FROM item WHERE i_color IN ('slate', 'blanched', 'burnished')) AND cs_item_sk = i_item_sk AND cs_sold_date_sk = d_date_sk
      AND d_year = 2001 AND d_moy = 2 AND cs_bill_addr_sk = ca_address_sk AND ca_gmt_offset = -5 GROUP BY i_item_id),
  ws AS (SELECT i_item_id, SUM(ws_ext_sales_price) total_sales FROM web_sales, date_dim, customer_address, item
    WHERE i_item_id IN (SELECT i_item_id FROM item WHERE i_color IN ('slate', 'blanched', 'burnished')) AND ws_item_sk = i_item_sk AND ws_sold_date_sk = d_date_sk
      AND d_year = 2001 AND d_moy = 2 AND ws_bill_addr_sk = ca_address_sk AND ca_gmt_offset = -5 GROUP BY i_item_id)
SELECT i_item_id, SUM(total_sales) total_sales FROM (SELECT * FROM ss UNION ALL SELECT * FROM cs UNION ALL SELECT * FROM ws) tmp1
GROUP BY i_item_id ORDER BY total_sales, i_item_id LIMIT 100"),
        (60, "WITH ss AS (SELECT i_item_id, SUM(ss_ext_sales_price) total_sales FROM store_sales, date_dim, customer_address, item
    WHERE i_item_id IN (SELECT i_item_id FROM item WHERE i_category IN ('Music')) AND ss_item_sk = i_item_sk AND ss_sold_date_sk = d_date_sk
      AND d_year = 1998 AND d_moy = 9 AND ss_addr_sk = ca_address_sk AND ca_gmt_offset = -5 GROUP BY i_item_id),
  cs AS (SELECT i_item_id, SUM(cs_ext_sales_price) total_sales FROM catalog_sales, date_dim, customer_address, item
    WHERE i_item_id IN (SELECT i_item_id FROM item WHERE i_category IN ('Music')) AND cs_item_sk = i_item_sk AND cs_sold_date_sk = d_date_sk
      AND d_year = 1998 AND d_moy = 9 AND cs_bill_addr_sk = ca_address_sk AND ca_gmt_offset = -5 GROUP BY i_item_id),
  ws AS (SELECT i_item_id, SUM(ws_ext_sales_price) total_sales FROM web_sales, date_dim, customer_address, item
    WHERE i_item_id IN (SELECT i_item_id FROM item WHERE i_category IN ('Music')) AND ws_item_sk = i_item_sk AND ws_sold_date_sk = d_date_sk
      AND d_year = 1998 AND d_moy = 9 AND ws_bill_addr_sk = ca_address_sk AND ca_gmt_offset = -5 GROUP BY i_item_id)
SELECT i_item_id, SUM(total_sales) total_sales FROM (SELECT * FROM ss UNION ALL SELECT * FROM cs UNION ALL SELECT * FROM ws) tmp1
GROUP BY i_item_id ORDER BY i_item_id, total_sales LIMIT 100"),
        (62, "SELECT w_warehouse_name, web_name,
  SUM(CASE WHEN (ws_sold_date_sk - d_date_sk <= 30) THEN 1 ELSE 0 END) AS d30,
  SUM(CASE WHEN (ws_sold_date_sk - d_date_sk > 30) AND (ws_sold_date_sk - d_date_sk <= 60) THEN 1 ELSE 0 END) AS d3160,
  SUM(CASE WHEN (ws_sold_date_sk - d_date_sk > 60) THEN 1 ELSE 0 END) AS d60
FROM web_sales, warehouse, web_site, date_dim WHERE d_month_seq BETWEEN 1200 AND 1200 + 11 AND ws_sold_date_sk = d_date_sk AND ws_warehouse_sk = w_warehouse_sk AND ws_web_site_sk = web_site_sk
GROUP BY w_warehouse_name, web_name ORDER BY w_warehouse_name, web_name LIMIT 100"),
        (65, "SELECT s_store_name, i_item_desc, sc.revenue, i_current_price, i_wholesale_cost, i_brand
FROM store, item, (SELECT ss_store_sk, AVG(revenue) AS ave FROM (SELECT ss_store_sk, ss_item_sk, SUM(ss_sales_price) AS revenue FROM store_sales, date_dim
      WHERE ss_sold_date_sk = d_date_sk AND d_month_seq BETWEEN 1176 AND 1176 + 11 GROUP BY ss_store_sk, ss_item_sk) sa GROUP BY ss_store_sk) sb,
  (SELECT ss_store_sk, ss_item_sk, SUM(ss_sales_price) AS revenue FROM store_sales, date_dim WHERE ss_sold_date_sk = d_date_sk AND d_month_seq BETWEEN 1176 AND 1176 + 11
    GROUP BY ss_store_sk, ss_item_sk) sc
WHERE sb.ss_store_sk = sc.ss_store_sk AND sc.revenue <= 0.1 * sb.ave AND s_store_sk = sc.ss_store_sk AND i_item_sk = sc.ss_item_sk
ORDER BY s_store_name, i_item_desc LIMIT 100"),
        (68, "SELECT c_last_name, c_first_name, ca_city, bought_city, ss_ticket_number, extended_price, extended_tax, list_price FROM (
  SELECT ss_ticket_number, ss_customer_sk, ca_city bought_city, SUM(ss_ext_sales_price) extended_price, SUM(ss_ext_list_price) list_price, SUM(ss_coupon_amt) extended_tax
  FROM store_sales, date_dim, store, household_demographics, customer_address
  WHERE store_sales.ss_sold_date_sk = date_dim.d_date_sk AND store_sales.ss_store_sk = store.s_store_sk AND store_sales.ss_hdemo_sk = household_demographics.hd_demo_sk
    AND store_sales.ss_addr_sk = customer_address.ca_address_sk AND date_dim.d_dom BETWEEN 1 AND 2 AND (household_demographics.hd_dep_count = 4 OR household_demographics.hd_vehicle_count = 3)
    AND date_dim.d_year IN (1999, 2000, 2001) AND store.s_city IN ('Midway', 'Fairview')
  GROUP BY ss_ticket_number, ss_customer_sk, ss_addr_sk, ca_city) dn, customer, customer_address current_addr
WHERE ss_customer_sk = c_customer_sk AND customer.c_current_addr_sk = current_addr.ca_address_sk AND current_addr.ca_city <> bought_city
ORDER BY c_last_name, ss_ticket_number LIMIT 100"),
        (70, "SELECT SUM(ss_net_profit) AS total_sum, s_state, s_county, GROUPING(s_state) + GROUPING(s_county) AS lochierarchy,
  RANK() OVER (PARTITION BY GROUPING(s_state) + GROUPING(s_county), CASE WHEN GROUPING(s_county) = 0 THEN s_state END ORDER BY SUM(ss_net_profit) DESC) AS rank_within_parent
FROM store_sales, date_dim d1, store WHERE d1.d_month_seq BETWEEN 1200 AND 1200 + 11 AND d1.d_date_sk = ss_sold_date_sk AND s_store_sk = ss_store_sk
  AND s_state IN (SELECT s_state FROM (SELECT s_state AS s_state, RANK() OVER (PARTITION BY s_state ORDER BY SUM(ss_net_profit) DESC) AS ranking
      FROM store_sales, store, date_dim WHERE d_month_seq BETWEEN 1200 AND 1200 + 11 AND d_date_sk = ss_sold_date_sk AND s_store_sk = ss_store_sk GROUP BY s_state) tmp1 WHERE ranking <= 5)
GROUP BY ROLLUP (s_state, s_county) ORDER BY lochierarchy DESC, CASE WHEN lochierarchy = 0 THEN s_state END, rank_within_parent LIMIT 100"),
        (73, "SELECT c_last_name, c_first_name, c_salutation, c_preferred_cust_flag, ss_ticket_number, cnt FROM (
  SELECT ss_ticket_number, ss_customer_sk, COUNT(*) cnt FROM store_sales, date_dim, store, household_demographics
  WHERE store_sales.ss_sold_date_sk = date_dim.d_date_sk AND store_sales.ss_store_sk = store.s_store_sk AND store_sales.ss_hdemo_sk = household_demographics.hd_demo_sk
    AND date_dim.d_dom BETWEEN 1 AND 2 AND (household_demographics.hd_buy_potential = '>10000' OR household_demographics.hd_buy_potential = 'Unknown')
    AND household_demographics.hd_vehicle_count > 0 AND CASE WHEN household_demographics.hd_vehicle_count > 0 THEN household_demographics.hd_dep_count / household_demographics.hd_vehicle_count ELSE NULL END > 1
    AND date_dim.d_year IN (1999, 2000, 2001) AND store.s_county IN ('Williamson County', 'Franklin Parish', 'Bronx County', 'Orange County')
  GROUP BY ss_ticket_number, ss_customer_sk) dj, customer
WHERE ss_customer_sk = c_customer_sk AND cnt BETWEEN 1 AND 5 ORDER BY cnt DESC, c_last_name ASC"),
        (79, "SELECT c_last_name, c_first_name, SUBSTR(s_city, 1, 30), ss_ticket_number, amt, profit FROM (
  SELECT ss_ticket_number, ss_customer_sk, store.s_city, SUM(ss_coupon_amt) amt, SUM(ss_net_profit) profit FROM store_sales, date_dim, store, household_demographics
  WHERE store_sales.ss_sold_date_sk = date_dim.d_date_sk AND store_sales.ss_store_sk = store.s_store_sk AND store_sales.ss_hdemo_sk = household_demographics.hd_demo_sk
    AND (household_demographics.hd_dep_count = 6 OR household_demographics.hd_vehicle_count > 2) AND date_dim.d_dow = 1 AND date_dim.d_year IN (1999, 2000, 2001)
    AND store.s_number_employees BETWEEN 200 AND 295 GROUP BY ss_ticket_number, ss_customer_sk, ss_addr_sk, store.s_city) ms, customer
WHERE ss_customer_sk = c_customer_sk ORDER BY c_last_name, c_first_name, SUBSTR(s_city, 1, 30), profit LIMIT 100"),
        (82, "SELECT i_item_id, i_item_desc, i_current_price FROM item, inventory, date_dim, store_sales
WHERE i_current_price BETWEEN 62 AND 62 + 30 AND inv_item_sk = i_item_sk AND d_date_sk = inv_date_sk AND d_date BETWEEN DATE '2000-05-25' AND DATE '2000-07-24'
  AND i_manufact_id IN (129, 270, 821, 423) AND inv_quantity_on_hand BETWEEN 100 AND 500 AND ss_item_sk = i_item_sk
GROUP BY i_item_id, i_item_desc, i_current_price ORDER BY i_item_id LIMIT 100"),
        (87, "SELECT COUNT(*) FROM ((SELECT DISTINCT c_last_name, c_first_name, d_date FROM store_sales, date_dim, customer
    WHERE store_sales.ss_sold_date_sk = date_dim.d_date_sk AND store_sales.ss_customer_sk = customer.c_customer_sk AND d_month_seq BETWEEN 1200 AND 1200 + 11)
  EXCEPT (SELECT DISTINCT c_last_name, c_first_name, d_date FROM catalog_sales, date_dim, customer
    WHERE catalog_sales.cs_sold_date_sk = date_dim.d_date_sk AND catalog_sales.cs_bill_customer_sk = customer.c_customer_sk AND d_month_seq BETWEEN 1200 AND 1200 + 11)
  EXCEPT (SELECT DISTINCT c_last_name, c_first_name, d_date FROM web_sales, date_dim, customer
    WHERE web_sales.ws_sold_date_sk = date_dim.d_date_sk AND web_sales.ws_bill_customer_sk = customer.c_customer_sk AND d_month_seq BETWEEN 1200 AND 1200 + 11)) cool_cust"),
        (89, "SELECT * FROM (SELECT i_category, i_class, i_brand, s_store_name, s_company_name, d_moy, SUM(ss_sales_price) sum_sales,
    AVG(SUM(ss_sales_price)) OVER (PARTITION BY i_category, i_brand, s_store_name, s_company_name) avg_monthly_sales
  FROM item, store_sales, date_dim, store WHERE ss_item_sk = i_item_sk AND ss_sold_date_sk = d_date_sk AND ss_store_sk = s_store_sk AND d_year IN (1999)
    AND ((i_category IN ('Books', 'Electronics', 'Sports') AND i_class IN ('computers', 'stereo', 'football')) OR (i_category IN ('Men', 'Jewelry', 'Women') AND i_class IN ('shirts', 'birdal', 'dresses')))
  GROUP BY i_category, i_class, i_brand, s_store_name, s_company_name, d_moy) tmp1
WHERE CASE WHEN avg_monthly_sales <> 0 THEN ABS(sum_sales - avg_monthly_sales) / avg_monthly_sales ELSE NULL END > 0.1
ORDER BY sum_sales - avg_monthly_sales, s_store_name LIMIT 100"),
        (92, "SELECT SUM(ws_ext_discount_amt) AS excess_discount_amount FROM web_sales, item, date_dim
WHERE i_manufact_id = 350 AND i_item_sk = ws_item_sk AND d_date BETWEEN DATE '2000-01-27' AND DATE '2000-04-26' AND d_date_sk = ws_sold_date_sk
  AND ws_ext_discount_amt > (SELECT 1.3 * AVG(ws_ext_discount_amt) FROM web_sales, date_dim WHERE ws_item_sk = i_item_sk AND d_date BETWEEN DATE '2000-01-27' AND DATE '2000-04-26' AND d_date_sk = ws_sold_date_sk)
ORDER BY SUM(ws_ext_discount_amt) LIMIT 100"),
        (93, "SELECT ss_customer_sk, SUM(act_sales) sumsales FROM (SELECT ss_item_sk, ss_ticket_number, ss_customer_sk,
    CASE WHEN sr_return_quantity IS NOT NULL THEN (ss_quantity - sr_return_quantity) * ss_sales_price ELSE ss_quantity * ss_sales_price END act_sales
  FROM store_sales LEFT OUTER JOIN store_returns ON (sr_item_sk = ss_item_sk AND sr_ticket_number = ss_ticket_number), reason
  WHERE sr_reason_sk = r_reason_sk AND r_reason_desc = 'reason 28') t GROUP BY ss_customer_sk ORDER BY sumsales, ss_customer_sk LIMIT 100"),
        (96, "SELECT COUNT(*) FROM store_sales, household_demographics, store WHERE ss_hdemo_sk = hd_demo_sk AND ss_store_sk = s_store_sk AND hd_dep_count = 7 AND s_store_name = 'ese' ORDER BY COUNT(*) LIMIT 100"),
        (97, "WITH ssci AS (SELECT ss_customer_sk customer_sk, ss_item_sk item_sk FROM store_sales, date_dim WHERE ss_sold_date_sk = d_date_sk AND d_month_seq BETWEEN 1200 AND 1200 + 11 GROUP BY ss_customer_sk, ss_item_sk),
  csci AS (SELECT cs_bill_customer_sk customer_sk, cs_item_sk item_sk FROM catalog_sales, date_dim WHERE cs_sold_date_sk = d_date_sk AND d_month_seq BETWEEN 1200 AND 1200 + 11 GROUP BY cs_bill_customer_sk, cs_item_sk)
SELECT SUM(CASE WHEN ssci.customer_sk IS NOT NULL AND csci.customer_sk IS NULL THEN 1 ELSE 0 END) store_only,
  SUM(CASE WHEN ssci.customer_sk IS NULL AND csci.customer_sk IS NOT NULL THEN 1 ELSE 0 END) catalog_only,
  SUM(CASE WHEN ssci.customer_sk IS NOT NULL AND csci.customer_sk IS NOT NULL THEN 1 ELSE 0 END) store_and_catalog
FROM ssci FULL OUTER JOIN csci ON (ssci.customer_sk = csci.customer_sk AND ssci.item_sk = csci.item_sk) LIMIT 100"),
        (98, "SELECT i_item_id, i_item_desc, i_category, i_class, i_current_price, SUM(ss_ext_sales_price) AS itemrevenue,
  SUM(ss_ext_sales_price) * 100 / SUM(SUM(ss_ext_sales_price)) OVER (PARTITION BY i_class) AS revenueratio
FROM store_sales, item, date_dim WHERE ss_item_sk = i_item_sk AND i_category IN ('Sports', 'Books', 'Home') AND ss_sold_date_sk = d_date_sk
  AND d_date BETWEEN DATE '1999-02-22' AND DATE '1999-03-24'
GROUP BY i_item_id, i_item_desc, i_category, i_class, i_current_price ORDER BY i_category, i_class, i_item_id, i_item_desc, revenueratio"),
    ]
}
