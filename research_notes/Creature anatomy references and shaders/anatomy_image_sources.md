# Anatomical Reference Image Sources for a Procedural Creature Generator (ornisculpt)

Scope: 2D images (plates, drawings, photos, diagrams) of animal skeleton, musculature, surface form and proportion. 3D scans are out of scope. Research date: 2026-10-19. Archive.org identifiers below were confirmed to exist via the archive.org advancedsearch API on that date (URL pattern `https://archive.org/details/<identifier>`); I did not open every item's viewer, so per-item download/lending status (open vs. "borrow"/controlled lending) is not verified unless stated.

General licensing rule used throughout (US-centric): works published in the US before 1931 are public domain in the US as of 2026 (95-year term; 1930 works entered PD on 2026-01-01). This rule is my inference from US copyright law, not from a source fetched in this session. Old works first published abroad can still be in copyright in their home country if the author died less than 70 years ago (EU/UK life+70).

## 1. Public-domain classic anatomy atlases: what is scanned and where

### Takeaway
The core veterinary/comparative atlases (Chauveau, Sisson pre-1931, Stubbs, Muybridge's 1887 plates, Wiedersheim) all exist as free scans on Internet Archive, BHL, Wellcome and NLM, and are safe to treat as public domain. The single most useful artist-oriented atlas, Ellenberger/Baum/Dittrich, is on archive.org only as an unofficial upload of the 1956 Dover edition with no rights statement, so its status is uncertain.

### Cited Findings

**Ellenberger, Baum & Dittrich: *An Atlas of Animal Anatomy for Artists* (Dover 1956 edition)**
- Contents: 288 drawings covering horse, dog, lion, cow/bull, stag and goat. Each animal is shown as an external view, the musculature under the skin, and the skeleton — [Dracut Library catalogue / Dover description](https://link.dracutlibrary.org/resource/aG3_x0u3BHQ); 151 pp., ISBN 0486200825 — [WA Museum library](https://library.museum.wa.gov.au/fullRecord.jsp?recno=8426). Credit for the editor varies between listings (Lewis S. Brown vs. Francis A. Davis) — same sources.
- Archive.org item: `an-atlas-of-animal-anatomy-for-artists-dover-anatomy-for-baum-hermann-brown-lewi` (dated 1956-06-01). It was uploaded by a private user (kingdomofthe0000@gmail.com), and the metadata has **no licenseurl and no rights field** — [archive.org metadata](https://archive.org/metadata/an-atlas-of-animal-anatomy-for-artists-dover-anatomy-for-baum-hermann-brown-lewi/metadata); item page: https://archive.org/details/an-atlas-of-animal-anatomy-for-artists-dover-anatomy-for-baum-hermann-brown-lewi
- Anatomy4Sculptors recommends using Ellenberger alongside Goldfinger, calling Goldfinger "schematic" by comparison — [Anatomy4Sculptors, Best anatomy books](https://anatomy4sculptors.com/blog/best-anatomy-books-for-artists/).
- LICENSE: **UNCERTAIN.** The underlying German *Handbuch der Anatomie der Tiere für Künstler* plates (c. 1898–1911) are very likely PD. The 1956 Dover edition adds English text and editorial matter that may carry its own copyright. Use the plates as visual reference only. Before committing any scan, look for an original German edition on BHL, HathiTrust or archive.org; that search was not done here (see Gaps).

**George Stubbs: *The Anatomy of the Horse* (1766)**
- Archive.org: `the-anatomy-of-the-horse-1766-1823` (https://archive.org/details/the-anatomy-of-the-horse-1766-1823) and `b33544049`, an 1815 edition (the "b"-number identifiers are Wellcome digitisations) (https://archive.org/details/b33544049) — archive.org search API.
- The Utrecht University Library has a full-text scan of the 1766 edition, "In Eighteen TABLES, all done from Nature" — [Utrecht objects.library.uu.nl](https://objects.library.uu.nl/download/1874-41001/fulltext). The College of Physicians of Philadelphia also has a digital copy — [CPP Digital Library](https://cppdigitallibrary.org/collections/show/12).
- The NLM states it "believes this item to be in the public domain" — [NLM Digital Collections 101456832](https://resource.nlm.nih.gov/101456832), [101456833](https://resource.nlm.nih.gov/101456833).
- The Met offers its Stubbs plates under Open Access (CC0: "freely copy, modify and distribute… even for commercial purposes") — [Met 359031](https://www.metmuseum.org/art/collection/search/359031).
- Wellcome's 1853 Bohn reprint (24 plates) is in closed stores, and I found no digitised copy of it — [Wellcome spt557n2](https://content.wellcomecollection.org/works/spt557n2).
- LICENSE: **Public domain.** The Met CC0 images are the cleanest choice to commit to a repo.
- Coverage: horse only. Skeleton plus successive muscle layers in lateral, anterior and posterior views. These are among the best-quality horse myology plates ever made.

**Chauveau & Arloing (tr. Fleming): *The Comparative Anatomy of the Domesticated Animals***
- Archive.org editions: 1873 `comparativeanato00chauuoft`, `comparativeana00chau`, `comparativeanato00chaurich`, `ags9990.0001.001.umich.edu`, `b21701337` (Wellcome); 1886 `cu31924000012389`; 1887 `comparativeanat00chaugoog`; 1891 `cu31924001093933`, `comparativeanat00chau`; 1908 `comparativeanato00chau` — archive.org search API.
- Wellcome scan of the 1873 Churchill edition: 1,036 images, from the Royal College of Physicians of Edinburgh — [Wellcome c7hg8ntg](https://wellcomecollection.org/works/c7hg8ntg). A direct PDF is at https://iiif.wellcomecollection.org/pdf/b21701337 — [Wellcome IIIF](https://iiif.wellcomecollection.org/pdf/b21701337).
- BHL records: 1908 Appleton 2nd English ed., DOI 10.5962/bhl.title.26920 — [BHL 26920](https://biodiversitylibrary.org/bibliography/26920); 1873 D. Appleton, DOI 10.5962/bhl.title.29617 — [BHL 29617](https://biodiversitylibrary.org/bibliography/29617).
- Cornell CHLA has the 1890 Appleton edition as 1,009 page images (300 dpi JPEG) — [Cornell chla2762661](https://digital.library.cornell.edu/catalog/chla2762661).
- LICENSE: **Public domain** (the 1908 and earlier editions predate 1931).
- Coverage: horse, ox, sheep, pig, dog, cat and some birds. Skeleton, joints, muscles and viscera in woodcut figures.

**Sisson: *A Text-book of Veterinary Anatomy* (1910/1911) and *The Anatomy of the Domestic Animals* (1914, 1917)**
- Archive.org: 1910 `textbookofveteri00sissrich`, `textbookofveter00siss`, `atextbookveteri00grosgoog`; 1911 `textbookofveteri00siss`, `cu31924000008742`, `textbookofveteri00siss_0`; 1914 `cu31924001263270`, `anatomyofdomesti01siss`, `anatomyofdomesti00sissuoft`, `anatomyofdomesti00siss`; 1917 `bwb_Y0-BAP-304` — archive.org search API.
- LICENSE: **Public domain** for the 1910–1917 editions. **Not PD:** Sisson & Grossman 1938/1945/1953/1955/1961/1975 (e.g. `anatomyofdomesti0000sept`, `sissongrossmansa0000siss`). These are almost certainly controlled-lending scans of in-copyright books, so use them as reference only.
- Coverage: horse, ox, pig, dog. Many photographic and drawn figures of bones and muscles, with good articulated-skeleton views.

**Eadweard Muybridge: locomotion photo sequences**
- 1887 *Animal Locomotion* (original): `animallocomotion00muyb`, `cu31924024580353`. 1893 *Descriptive Zoopraxography*: `descriptivezoopr00muyb`, `descriptivezoopr00muyb_0`, `descriptivezoopr00muyb_1`, `b22651901`, `b29330531`. 1891 *Science of Animal Locomotion*: `scienceofanimall00muyb`. 1882 *Attitudes of Animals in Motion*: `attitudesofanima00muyb`. 1902 *Animals in Motion*: `animalsmotion00muyb`, `gri_33125012264293` (Getty). 1888 UPenn method volume: `animallocomotion00univ_0`. Project Gutenberg-derived texts: `descriptivezoopr40215gut`, `thescienceofanim39998gut`, `theattitudesofan37743gut` — archive.org search API.
- Stillman (Muybridge photos), *The Horse in Motion* (1882): `horseinmotionass00stil`, `horseinmotionass00stilrich`, `horseinmotionass01stil`, `b2191123x` — archive.org search API.
- Dover reprints (1955 *Human Figure in Motion*, 1957 *Animals in Motion*, 1979 *Complete Human and Animal Locomotion*, 781 plates, `muybridgescomple01muyb` to `03muyb`) appear on archive.org as lending copies — archive.org search API.
- LICENSE: **Public domain** for the 1887–1902 originals. The photographs inside the Dover reprints are also PD, but scans of the Dover books carry the Dover layout and introductions. Take images from the 1887/1902 scans or from Wikimedia Commons rather than from the Dover reprints.
- Coverage: horse gaits, dog, cat, lion, elephant, camel, deer, bison, pig, birds in flight (pigeon, cockatoo, vulture) and humans. Essential for gait cycles and limb timing.

**Wiedersheim: *Elements of the Comparative Anatomy of Vertebrates* (1886, 1897, 1907)**
- Archive.org: `elementsofcompar00wieduoft` (1886), `elementscompara00detwgoog` (1886), `comparelementsof00wiedrich` (1897), `comparativeanat00wiedgoog` (1907), `comparativeanato00wiedrich` (1907) — archive.org search API.
- LICENSE: **Public domain.** Coverage: vertebrate skeleton and muscle across fish, reptiles, birds and mammals (girdles, limbs, skulls). Useful for the lizard and bird archetypes.

**Birds (classic)**
- Newton, Gadow, Lydekker, Roy & Shufeldt, *A Dictionary of Birds* (1893): `dictionaryofbird00newt`. Gadow's anatomy entries include pterylosis and skeleton figures — archive.org search API. **PD.**
- R. W. Shufeldt's bird osteology papers (1888–1915) are on archive.org via BioStor, e.g. `biostor-121647` "Recent Contributions to… the Osteology of Birds" — archive.org search API. Shufeldt also wrote *The osteology of Vulpes macrotis* (1900), `101687733.nlm.nih.gov` (fox skeleton) — same source. **PD.**
- Nitzsch's *Pterylography* (Ray Society no. 41, 1867; tr. W. S. Dallas, ed. P. L. Sclater; 181 pp., 10 plates, 37 cm): the definitive classic on feather tracts — [NLI catalogue](https://catalogue.nli.ie/Record/vtls000380929), [WA Museum](https://library.museum.wa.gov.au/fullRecord.jsp?recno=6916). **PD** (1867), but no online scan was located in this session (see Gaps).

**Insects (classic)**
- Snodgrass, *Anatomy and Physiology of the Honey Bee* (1925): archive.org `dli.ernet.5654` — archive.org search API. **PD in the US** (pre-1931).
- Snodgrass, *Insects: Their Ways and Means of Living* (1930, Smithsonian Scientific Series vol. 5): `bwb_O7-ACJ-729` (a lending copy) — archive.org search API. The 1930 printing is PD in the US as of 2026; this is my inference.
- Snodgrass, *Principles of Insect Morphology* (McGraw-Hill 1935, 667 pp.) is the standard insect morphology diagram source. A PDF is on Zenodo, uploaded by Plazi, licensed "Other (Open)" — [Zenodo 54861](https://zenodo.org/records/54861). **License UNCERTAIN:** a 1935 US work is PD only if its copyright was not renewed, and this was not checked. Cornell University Press reissued it in 1994 — [OEAW record](https://oeaw.ac.at/resources/Record/9781501717918).

**Wellcome Collection (general)**
- Most Wellcome images carry either a Creative Commons licence or the Public Domain Mark. The CC variants range from CC-BY to CC-BY-NC-ND. The suggested credit is "Wellcome Collection", and PD items need no attribution — [Wellcome copyright guidance](https://docs.wellcomecollection.org/collections-information/lQ0fMyP8xr8hWWJiQkfl/enquiries/images-and-copyright/copyright-images). Each item states its licence (example PDM record: [Wellcome fyr6hk6c](https://wellcomecollection.org/works/fyr6hk6c)).

### Inferences
- The best starting set for the existing archetypes in `assets/mobs`: Stubbs and Ellenberger plates for horse, beast and quadruped (horse, dog, lion); Chauveau and Sisson pre-1931 for the domestic quadruped skeleton and myology; Muybridge 1887 for gaits; Wiedersheim and Gadow for lizard and bird girdles and limbs.
- Before saving any page image into the repo, prefer scans with an explicit PD or CC0 statement (Met, NLM, Wellcome PDM, Smithsonian CC0) over unofficial archive.org uploads.

### Gaps
- Ellenberger: I did not find an original German edition (*Handbuch der Anatomie der Tiere für Künstler*) on BHL, HathiTrust or archive.org, and I did not check whether the Dover edition's copyright was renewed.
- No online scan of Nitzsch's *Pterylography* surfaced. The BHL search was indirect. Try `biodiversitylibrary.org/search?searchTerm=Pterylography` and HathiTrust.
- Owen's *On the Anatomy of Vertebrates* (1866–68) and Knight's skeleton plates were not specifically located. The archive.org query for "anatomy of vertebrates" returned only Wiedersheim.
- I did not verify which archive.org items are freely downloadable and which are lending-only.

## 2. Modern artist resources and their licensing

### Takeaway
Modern artist books and courses (Goldfinger, Whitlatch, Anatomy4Sculptors, Proko, Blaise, Eaton) are all-rights-reserved commercial products. They are fine for study but must never go in the repo. Online photo-reference sites vary: ADW is per-image and mostly educational fair use, and Line of Action's terms were not found.

### Cited Findings
- **Eliot Goldfinger, *Animal Anatomy for Artists: The Elements of Form*** (Oxford UP, 2004) covers horses, dogs, lions, elephants and others. It focuses on the muscle and bone forms beneath the surface, and its drawings are schematic — [Anatomy4Sculptors](https://anatomy4sculptors.com/blog/best-anatomy-books-for-artists/), [eBay listing w/ publisher data](https://www.ebay.com/p/2851660). License: **all rights reserved**, reference only.
- **Terryl Whitlatch**: *Animals Real and Imagined* and its follow-up *Science of Creature Design: Understanding Animal Anatomy*. She trained in zoology and worked for Lucasfilm and Disney. The books focus on anatomy for creature design and directly fit the dragon and chimera work — [Stuart Ng Books](https://stuartngbooks.com/products/science-of-creature-design-understanding-animal-anatomy). License: **all rights reserved**.
- **Anatomy4Sculptors**: commercial books and blog. Its blog compares the major anatomy books — [Anatomy4Sculptors blog](https://anatomy4sculptors.com/blog/best-anatomy-books-for-artists/). **All rights reserved.**
- **Animal Diversity Web (University of Michigan)**: licensing is set per image. Typical wording: the image "may not be downloaded and used without permission of the copyright holder except for educational fair use". Some collections (e.g. Michigan Science Art) bar all use without permission — [ADW image page example](https://animaldiversity.org/collections/contributors/michael_kielb/pcd3942_018/), [ADW Grzimek example](https://animaldiversity.org/collections/contributors/Grzimek_inverts/Cladocera/Daphnia_pulex/). The BioKIDS/ADW use conditions restrict material to educational, non-commercial use and forbid mirroring (page dated 2001–2008, possibly outdated) — [BioKIDS use conditions](https://biokids.umich.edu/about/use_conditions). The ADW skeleton and skull photo collections are useful for viewing, but their images cannot go in the repo.
- **Line of Action**: a timed-drawing tool with animal sets. Photographer terms live in a "photo bundle and profit sharing" policy that I did not find. Its photographer page is https://line-of-action.com/photographers — [Line of Action photographers](https://line-of-action.com/photographers). License: **unknown, treat as all rights reserved.**
- **Animal Photo Reference Repository** (a separate free site, cited via a roundup): artists making derivative or transformative works without AI have "blanket permission… even for work they're going to sell". Other uses need permission — [Chris Wilson Studio roundup](https://chriswilsonstudio.com/animal-drawing-reference/). This permission covers drawing from the photos, not redistributing them.
- **PhyloPic** (phylopic.org): silhouettes, each under its own CC deed or mark. PDM and CC0 images can be used "anywhere, in any way, without attribution". CC BY needs credit and a licence link. Search can filter by licence, and an API exists — [PhyloPic usage](https://www.phylopic.org/articles/image-usage), [rphylopic vignette](https://cran.nics.utk.edu/cran/web/packages/rphylopic/vignettes/a-getting-started.html). Useful for proportion and outline checks across taxa.

### Inferences
- Proko, Aaron Blaise (Creature Art Teacher), Scott Eaton, Quickposes, Skulls Unlimited and BoneID were not individually checked. As paid courses or commercial photo sites they should be presumed all-rights-reserved. I found no evidence of any open licence.
- Drawing from these sources is allowed. Studying a photo, measuring its proportions and encoding them as numbers in `.body.ron` files does not copy protected expression. Bone-length ratios are facts and not copyrightable (my inference, not a cited legal opinion).

### Gaps
- Line of Action and Quickposes licence text, and the terms of Skulls Unlimited, BoneID, Proko and Blaise, were not retrieved.

## 3. Bird, bat, reptile and insect specific sources

### Takeaway
Use Gadow (in Newton's *Dictionary*), Shufeldt and Nitzsch for bird skeleton and feather tracts, Wiedersheim for reptile girdles and limbs, Snodgrass for insects, and Muybridge 1887 for bird flight sequences. Modern open-access papers (CC BY, from PLoS and PeerJ) via Wikimedia Commons are the best legally clean source for bat and pterosaur wing figures.

### Cited Findings
- Bird: *Dictionary of Birds* (1893, Gadow anatomy articles) `dictionaryofbird00newt`; Shufeldt osteology papers `biostor-121647`, `biostor-123398` (the sternum in the solitary sandpiper) — archive.org search API. Nitzsch's *Pterylography* (1867) covers feather tracts and pterylae (no scan located) — [NLI](https://catalogue.nli.ie/Record/vtls000380929).
- Bird flight photo sequences are in Muybridge's 1887 *Animal Locomotion* (`animallocomotion00muyb`) — archive.org search API.
- Reptile: Wiedersheim 1886–1907 scans (listed in Section 1) — archive.org search API.
- Insect: Snodgrass 1925 bee (`dli.ernet.5654`, PD); Snodgrass 1935 *Principles* on Zenodo (licence uncertain) — [Zenodo](https://zenodo.org/records/54861).
- BHL's Flickr stream holds more than 300,000 freely reusable nature illustrations and is searchable by scientific name. Most are public domain, but each image's status must be checked — [Illinois BHL Flickr guide](https://guides.library.illinois.edu/BHL/flickr), [CofC library guide](https://libguides.library.cofc.edu/marine-species-authorities/early-illustrations).

### Inferences
- BHL Flickr is likely the fastest way to find scale, plate, beetle and dragonfly engravings, since many are cropped single plates already tagged with taxon names.

### Gaps
- I found no specific open-licensed bat wing anatomy plate (e.g. plagiopatagium and finger-bone layout). The next step is a Wikimedia Commons search for "Chiroptera skeleton" or "bat wing anatomy", filtered to PD or CC-BY.
- I found no specific open source on lizard scale morphology (scalation diagrams).

## 4. Dragon/wyvern plausibility: pterosaur, bat and skeletal-diagram references

### Takeaway
Mark Witton's pterosaur figures on Wikimedia Commons are mostly CC BY (2.5 or 4.0), taken from PLoS ONE and PeerJ papers. They can legally be used, and even committed with attribution. Scott Hartman's skeletals are licensed per image: on PhyloPic some are CC BY 3.0 and others CC BY-NC-SA 3.0, while his website skeletals are copyrighted.

### Cited Findings
- Witton & Habib, Pteranodon quadrupedal launch figure (2010): CC BY 2.5. Hatzegopteryx and Arambourgiania reconstructions (Witton & Naish 2017, PeerJ): CC BY 4.0. Azhdarchid wing-shape figure (Witton & Naish 2008, PLoS ONE): CC BY 2.5. Istiodactylus skull (2012, PLoS ONE): CC BY 2.5 — [Vikidia mirror of Commons file](https://en.vikidia.org/wiki/File:Hatzegopteryx-Witton-and-Naish-2017.png); [Wikipedia-for-schools mirror](https://solarspell-dls.sfis.asu.edu/mea/wikipedia/images/2076/207693.jpg.htm). These are secondary mirrors, so confirm on the original commons.wikimedia.org file page.
- A Witton Effigia image on EurekAlert lists "none" under usage restrictions but names no licence, so it is **unclear** — [EurekAlert 809297](https://www.eurekalert.org/multimedia/809297).
- Scott Hartman on PhyloPic: CC BY 3.0 for Massospondylus, Iguanodon, Heterodontosaurus and Phenacodontidae. CC BY-NC-SA 3.0 for Alamosaurus, Silesaurus and Parasaurolophus — [PhyloPic Massospondylus](https://www.phylopic.org/images/2da08fc6-7dc0-4d20-813c-58069902a96b/massospondylus-carinatus), [PhyloPic Alamosaurus](https://www.phylopic.org/images/8d57a169-176d-448f-81ef-442adfed1556/alamosaurus-sanjuanensis). A Talos skeletal on EurekAlert is marked "copyright Scott Hartman… used with permission" — [EurekAlert 628543](https://www.eurekalert.org/multimedia/628543).
- Whitlatch's *Science of Creature Design* covers anatomy for fictional creatures (all rights reserved) — [Stuart Ng](https://stuartngbooks.com/products/science-of-creature-design-understanding-animal-anatomy).

### Inferences
- PLoS ONE and PeerJ publish under CC BY by default. Searching them for "pterosaur wing", "bat wing membrane" or "flight muscle" is a legally clean way to get modern diagrams.
- Hartman's full-resolution skeletals on skeletaldrawing.com should be presumed all-rights-reserved. They are fine for visual reference but should not be redistributed.

### Gaps
- Hartman's own licence statement (website and Patreon) was not retrieved.
- No dedicated speculative-dragon-anatomy writing was sourced. Candidates for follow-up are Darren Naish's Tetrapod Zoology and the Witton blog posts on azhdarchid launch mechanics.

## 5. Open-access museum and library image APIs for bulk or programmatic access

### Takeaway
Smithsonian (CC0, api.data.gov key plus a weekly AWS S3 dump), Wikimedia Commons (MediaWiki API with `extmetadata` licence fields), Europeana (`reusability=open` filter), BHL (API plus Flickr) and PhyloPic (API with a per-image licence field) all support programmatic access. Wellcome serves IIIF images and PDFs.

### Cited Findings
- **Smithsonian Open Access**: the metadata API is on api.data.gov and needs a key. Field, department and data-type docs are served by the API itself, and a GitHub repo of JSON data refreshes weekly. Media is provided only for PD works, under CC0 — [SI devtools](https://www.si.edu/openaccess/devtools). There is a bulk S3 bucket `smithsonian-open-access` (us-west-2), updated weekly, with no AWS account needed — [AWS Open Data registry](https://registry.opendata.aws/smithsonian-open-access). CC0 covers copyright only, so other permissions may still apply — [SI Open Access FAQ](https://dashboard.si.edu/openaccess/faq). The 2020 launch released 2.8M CC0 images — [Creative Commons](https://creativecommons.org/2020/02/27/smithsonian-releases-2-8-million-images-data-into-the-public-domain-using-cc0/). The endpoint `https://api.si.edu/openaccess/api/v1.0/search` appears only on a third-party page and was not confirmed against official docs — [archivarix mirror](https://echo.archivarix.net/es/archive/smithsonian-openaccess).
- **Wikimedia Commons**: `prop=imageinfo&iiprop=extmetadata` returns licence and attribution fields parsed from the file description templates — [Extension:CommonsMetadata](https://www.mediawiki.org/wiki/Extension:CommonsMetadata/en). Openverse harvests in bulk with `generator=allimages` plus extmetadata, paging with `continue` — [Openverse Commons provider](https://make.wordpress.org/openverse/handbook/openverse-catalog/provider-api-wikimedia-commons/). A third-party tool exposes the field `LicenseShortName` (e.g. "CC BY-SA 4.0"); verify this against a live response — [haykranen.nl](https://www.haykranen.nl/?p=5702).
- **Europeana Search API**: `reusability=open|restricted|permission`, where open covers PD, CC0, CC BY and CC BY-SA. Combine with `qf=TYPE:IMAGE` and `media=true`, and pass the key as `wskey`. Results max out at 100 rows per request — [roued.com explainer](https://roued.com/portfolio/europeanas-api-explained/), [polymatheia docs](https://polymatheia.readthedocs.io/en/stable/recipes/europeana.html). These are not the official docs, so verify parameter names.
- **BHL**: offers APIs and data exports — [About BHL](https://about.biodiversitylibrary.org/). The images on Flickr are searchable by taxon — [Illinois guide](https://guides.library.illinois.edu/BHL/flickr). Commons also mirrors BHL content: [Commons:Biodiversity Heritage Library](https://c.enwp.org/wiki/Commons:Biodiversity_Heritage_Library).
- **Wellcome**: images are served over IIIF in the pattern `iiif.wellcomecollection.org/image/<id>.jp2/full/640,/0/default.jpg` (inferred from a catalogue page, not the official docs). PDFs are at `iiif.wellcomecollection.org/pdf/<b-number>` — [Wellcome search result](https://wellcomecollection.org/works/ak5ffkgu/items), [Chauveau PDF](https://iiif.wellcomecollection.org/pdf/b21701337).
- **Internet Archive**: `archive.org/advancedsearch.php?...&output=json` and `archive.org/metadata/<id>` both worked without a key in this session and were used to compile the identifiers above.
- **PhyloPic**: the API returns a per-image licence field, e.g. PDM — [rphylopic vignette](https://cran.nics.utk.edu/cran/web/packages/rphylopic/vignettes/a-getting-started.html).
- **The Met**: its Open Access images are CC0 (Stubbs example) — [Met 359031](https://www.metmuseum.org/art/collection/search/359031). The Met's API was not researched here.

### Inferences
- The Smithsonian S3 dump combined with a `unit_code`/taxon filter (e.g. the NMNH Vertebrate Zoology specimen photos) is the most scalable CC0 source of skeleton photos. I have not verified that the coverage is useful.

### Gaps
- BHL API v3 key registration details and the official Europeana and Smithsonian docs pages were not opened.

## 6. Practical licensing guidance: what may be committed to the repo

### Takeaway
Commit only PD, CC0 or CC BY images (CC BY needs an attribution file). Keep everything all-rights-reserved, NC or unknown out of the repo and use it as local, uncommitted reference.

### Cited Findings
- PDM and CC0 images can be used without attribution — [PhyloPic usage](https://www.phylopic.org/articles/image-usage); [Wellcome copyright](https://docs.wellcomecollection.org/collections-information/lQ0fMyP8xr8hWWJiQkfl/enquiries/images-and-copyright/copyright-images).
- CC BY requires crediting the author, linking the licence and indicating changes. NC-SA forbids commercial use and requires derivatives to use the same licence — [PhyloPic Hartman pages](https://www.phylopic.org/images/8d57a169-176d-448f-81ef-442adfed1556/alamosaurus-sanjuanensis).
- CC0 waives copyright only, and other rights may still apply — [SI FAQ](https://dashboard.si.edu/openaccess/faq).
- Crediting BHL Flickr is good practice even for PD images — [CofC guide](https://libguides.library.cofc.edu/marine-species-authorities/early-illustrations).
- Some commentators dispute whether Wellcome can claim CC-BY on faithful digitisations of PD works, so check each item's stated licence — [Wellcome search summary / OKFN list](https://lists-archive.okfn.org/pipermail/open-education/2014-January/003586.html).

### Inferences (practical recommendations, not legal advice)
- **Tier A (commit freely)**: Met CC0, Smithsonian CC0, NLM PD, Wellcome PDM, pre-1931 archive.org/BHL plates (Chauveau, Sisson 1910–1917, Stubbs, Muybridge 1887, Wiedersheim, Gadow/Shufeldt), PhyloPic CC0/PDM. Still add a `research_notes/` or `assets/reference/SOURCES.md` listing the source URL for each file.
- **Tier B (commit with attribution)**: CC BY and CC BY-SA (Witton figures, CC BY Hartman PhyloPic silhouettes, Wellcome CC-BY). An SA licence applies to the image and its adaptations. It does not affect the Rust code, but keep these files in a separate folder with their licence text.
- **Tier C (do not commit; local reference only)**: CC BY-NC(-SA) (some Hartman silhouettes), ADW, Goldfinger, Whitlatch, Anatomy4Sculptors, Proko, Blaise, Eaton, Line of Action, Sisson & Grossman 1938+, Dover reprints, and the Ellenberger archive.org upload until its status is confirmed. Add a `.gitignore`d `reference_local/` folder for these.
- Measurements taken from any source (bone ratios, joint ranges) and encoded as numbers in `.body.ron` files are facts and safe to commit. Tracing an image into a committed SVG or texture creates a derivative and falls under the source's licence.

### Gaps
- Non-US jurisdictions were not analysed (EU sui generis rules, and the UK position on faithful reproductions after *THJ v Sheridan*).
